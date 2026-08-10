use std::path::{Path, PathBuf as StdPathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::keymap::{self, Action, MatchResult, PaneDir};
use crate::pane::Pane;

// ── Tab completion ────────────────────────────────────────────────────────────

struct CompletionState {
    /// The part of command_buf before the path (e.g. `"e "`).
    cmd_prefix: String,
    candidates: Vec<String>,
    idx: usize,
}

/// List filesystem entries whose names start with `partial`'s basename,
/// searching in `partial`'s directory (or `.` if there is none).
fn path_completions(partial: &str) -> Vec<String> {
    let (search_dir, name_prefix): (StdPathBuf, String) = if partial.ends_with('/') {
        (StdPathBuf::from(partial), String::new())
    } else {
        let p = Path::new(partial);
        let dir = match p.parent() {
            Some(d) if d != Path::new("") => d.to_path_buf(),
            _ => StdPathBuf::from("."),
        };
        let prefix = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        (dir, prefix)
    };

    let mut results: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&search_dir) {
        for entry in entries.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            // Skip hidden entries unless the user is explicitly typing a dot
            if name.starts_with('.') && !name_prefix.starts_with('.') {
                continue;
            }
            if !name.starts_with(&name_prefix) {
                continue;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            // Reconstruct the full path string to return
            let completed = if search_dir == std::path::Path::new(".") && !partial.starts_with("./")
            {
                if is_dir {
                    format!("{}/", name)
                } else {
                    name
                }
            } else {
                let dir_s = search_dir.to_string_lossy();
                if is_dir {
                    format!("{}/{}/", dir_s, name)
                } else {
                    format!("{}/{}", dir_s, name)
                }
            };
            results.push(completed);
        }
    }
    // Directories first, then alphabetically within each group
    results.sort_by(|a, b| b.ends_with('/').cmp(&a.ends_with('/')).then(a.cmp(b)));
    results
}

/// Split a file-opening command like `"e notes/foo"` into `("e ", "notes/foo")`.
/// Returns `None` if the command doesn't take a file argument.
fn split_file_command(cmd: &str) -> Option<(String, String)> {
    for verb in &["edit!", "edit", "e!", "e", "w"] {
        if let Some(rest) = cmd.strip_prefix(verb) {
            if rest.is_empty() || rest.starts_with(' ') {
                let cmd_prefix = format!("{} ", verb);
                let partial = rest.trim().to_string();
                return Some((cmd_prefix, partial));
            }
        }
    }
    None
}

// ── Mode ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Command,
    Search,
    /// `line_wise = true` → V (line visual), false → v (char visual)
    Visual {
        line_wise: bool,
    },
    /// Minimal directory browser (`:e <dir>`, `:Ex`).
    Browse,
    /// Buffer switcher (`SPC b b`).
    BufferList,
}

impl Mode {
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Command => "COMMAND",
            Mode::Search => "SEARCH",
            Mode::Visual { line_wise: true } => "V-LINE",
            Mode::Visual { line_wise: false } => "VISUAL",
            Mode::Browse => "BROWSE",
            Mode::BufferList => "BUFFERS",
        }
    }
}

// ── Directory browser ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BrowseEntry {
    pub name: String,
    pub is_dir: bool,
}

// ── Window layout tree ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    /// Horizontal dividing line — panes stacked top/bottom (vim's `:split`, `C-w s`).
    Horizontal,
    /// Vertical dividing line — panes side by side (vim's `:vsplit`, `C-w v`).
    Vertical,
}

/// Recursive window tree. Leaves hold an index into `App::panes`; splits nest
/// freely so hsplit and vsplit can combine (e.g. one tall pane on the left,
/// two stacked panes on the right) and any number of panes is supported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneLayout {
    Leaf(usize),
    Split {
        dir: SplitDir,
        children: Vec<PaneLayout>,
    },
}

impl PaneLayout {
    /// Pane indices in left-to-right / top-to-bottom tree order.
    pub fn leaves(&self) -> Vec<usize> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves(&self, out: &mut Vec<usize>) {
        match self {
            PaneLayout::Leaf(idx) => out.push(*idx),
            PaneLayout::Split { children, .. } => {
                for c in children {
                    c.collect_leaves(out);
                }
            }
        }
    }

    /// Replace the leaf holding `target` with a new split of `[target, new_idx]`.
    fn split_leaf(&mut self, target: usize, new_idx: usize, dir: SplitDir) -> bool {
        match self {
            PaneLayout::Leaf(idx) if *idx == target => {
                *self = PaneLayout::Split {
                    dir,
                    children: vec![PaneLayout::Leaf(target), PaneLayout::Leaf(new_idx)],
                };
                true
            }
            PaneLayout::Leaf(_) => false,
            PaneLayout::Split { children, .. } => children
                .iter_mut()
                .any(|c| c.split_leaf(target, new_idx, dir)),
        }
    }

    /// Remove the leaf holding `target`, collapsing any split left with a
    /// single child so the tree stays minimal.
    fn remove_leaf(&mut self, target: usize) {
        if let PaneLayout::Split { children, .. } = self {
            children.retain(|c| c != &PaneLayout::Leaf(target));
            for child in children.iter_mut() {
                child.remove_leaf(target);
            }
            if children.len() == 1 {
                *self = children.remove(0);
            }
        }
    }

    /// Decrement every leaf index greater than `removed_idx` by one, to stay
    /// in sync after `App::panes.remove(removed_idx)`.
    fn renumber_after_removal(&mut self, removed_idx: usize) {
        match self {
            PaneLayout::Leaf(idx) => {
                if *idx > removed_idx {
                    *idx -= 1;
                }
            }
            PaneLayout::Split { children, .. } => {
                for c in children.iter_mut() {
                    c.renumber_after_removal(removed_idx);
                }
            }
        }
    }
}

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    pub mode: Mode,
    pub panes: Vec<Pane>,
    pub active_pane: usize,
    pub layout: PaneLayout,

    /// Shared yank register.
    pub register: Vec<String>,

    // Visual mode state
    pub visual_anchor: Option<(usize, usize)>, // (row, col) where v/V was pressed

    // Multi-key sequence accumulator (normal mode)
    pub key_seq: Vec<KeyEvent>,

    // Command mode input
    pub command_buf: String,

    // Status message (clears on next keypress)
    pub message: Option<String>,

    pub should_quit: bool,

    /// Pane rects as computed by the last render pass — used for mouse click mapping.
    pub pane_rects: Vec<Rect>,

    /// Active tab-completion session (command mode only).
    completions: Option<CompletionState>,

    // Search state
    pub search_buf: String,
    /// All (row, col) match positions in the active pane.
    pub search_matches: Vec<(usize, usize)>,
    pub search_match_idx: usize,
    /// Cursor position at the moment `/` was pressed; restored on Esc.
    search_origin: (usize, usize),

    /// When true (default), `[[url][label]]` links display just the label.
    /// The raw target is revealed only while the cursor's line is being
    /// edited in Insert mode; Normal-mode navigation never pops links open.
    /// Toggle with `SPC t l`.
    pub conceal_links: bool,

    // Directory browser state (Mode::Browse)
    pub browse_dir: StdPathBuf,
    pub browse_entries: Vec<BrowseEntry>,
    pub browse_selected: usize,
    /// First visible entry index (vertical scroll), kept in sync by the renderer.
    pub browse_scroll: usize,

    /// Most-recently-used list of file paths opened this session (front = most recent).
    pub buffers: Vec<StdPathBuf>,
    /// Selection state for Mode::BufferList.
    pub buf_list_selected: usize,
    pub buf_list_scroll: usize,
}

fn is_web_url(url: &str) -> bool {
    url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("ftp://")
        || url.starts_with("mailto:")
}

fn open_url(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener).arg(url).spawn();
}

impl App {
    pub fn new(file_path: Option<&str>) -> anyhow::Result<Self> {
        let pane = match file_path {
            Some(p) => Pane::from_file(p)?,
            None => Pane::empty(),
        };
        let buffers = match &pane.file_path {
            Some(p) => vec![p.clone()],
            None => Vec::new(),
        };
        Ok(Self {
            mode: Mode::Normal,
            panes: vec![pane],
            active_pane: 0,
            layout: PaneLayout::Leaf(0),
            register: Vec::new(),
            visual_anchor: None,
            key_seq: Vec::new(),
            command_buf: String::new(),
            message: None,
            should_quit: false,
            pane_rects: Vec::new(),
            completions: None,
            search_buf: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
            search_origin: (0, 0),
            conceal_links: true,
            browse_dir: StdPathBuf::new(),
            browse_entries: Vec::new(),
            browse_selected: 0,
            browse_scroll: 0,
            buffers,
            buf_list_selected: 0,
            buf_list_scroll: 0,
        })
    }

    // ── Pane access ───────────────────────────────────────────────────────────

    pub fn pane(&self) -> &Pane {
        &self.panes[self.active_pane]
    }

    pub fn pane_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.active_pane]
    }

    // ── Top-level input dispatch ──────────────────────────────────────────────

    pub fn handle_key(&mut self, key: KeyEvent) {
        self.message = None;
        match &self.mode.clone() {
            Mode::Normal => self.handle_normal(key),
            Mode::Insert => self.handle_insert(key),
            Mode::Command => self.handle_command(key),
            Mode::Search => self.handle_search(key),
            Mode::Visual { line_wise } => self.handle_visual(key, *line_wise),
            Mode::Browse => self.handle_browse(key),
            Mode::BufferList => self.handle_buffer_list(key),
        }
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.click_at(event.column as usize, event.row as usize);
            }
            MouseEventKind::ScrollDown => {
                let pane = &mut self.panes[self.active_pane];
                pane.scroll_down(3);
            }
            MouseEventKind::ScrollUp => {
                let pane = &mut self.panes[self.active_pane];
                pane.scroll_up(3);
            }
            _ => {}
        }
    }

    fn click_at(&mut self, col: usize, row: usize) {
        // Find which pane the click landed in
        for (i, rect) in self.pane_rects.iter().enumerate() {
            if col >= rect.x as usize
                && col < (rect.x + rect.width) as usize
                && row >= rect.y as usize
                && row < (rect.y + rect.height) as usize
            {
                self.active_pane = i;
                let pane = &mut self.panes[i];
                // Content starts after the 1-col git gutter (when there is room).
                const GUTTER: usize = 1;
                let content_x = if rect.width as usize > GUTTER {
                    rect.x as usize + GUTTER
                } else {
                    rect.x as usize
                };
                let buf_row = (pane.viewport_top + row - rect.y as usize)
                    .min(pane.lines.len().saturating_sub(1));
                let buf_col = (pane.viewport_left + col.saturating_sub(content_x))
                    .min(pane.lines[buf_row].len().saturating_sub(1));
                pane.cursor_row = buf_row;
                pane.cursor_col = buf_col;
                // Leave visual mode on click
                if matches!(self.mode, Mode::Visual { .. }) {
                    self.mode = Mode::Normal;
                    self.visual_anchor = None;
                }
                break;
            }
        }
    }

    // ── Normal mode ───────────────────────────────────────────────────────────

    fn handle_normal(&mut self, key: KeyEvent) {
        self.key_seq.push(key);
        let seq_str = keymap::seq_to_str(&self.key_seq);

        match keymap::match_seq(&seq_str) {
            MatchResult::Prefix => {
                let hint = keymap::hint_for_prefix(&seq_str);
                self.message = Some(format!("[{}]  {}", seq_str, hint));
                // keep accumulating
            }
            MatchResult::Action(action) => {
                self.key_seq.clear();
                self.dispatch_action(action);
            }
            MatchResult::NoMatch => {
                let was_accumulating = self.key_seq.len() > 1;
                let solo_key = self.key_seq.remove(0);
                self.key_seq.clear();
                if !was_accumulating {
                    self.handle_single_normal(solo_key);
                }
                // If was_accumulating: discard the failed sequence
            }
        }
    }

    /// Handle single-key normal-mode bindings (not part of a multi-key sequence).
    fn handle_single_normal(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // ── Movement ─────────────────────────────────────────────────────
            KeyCode::Char('h') | KeyCode::Left => self.pane_mut().move_left(),
            KeyCode::Char('j') | KeyCode::Down => self.pane_mut().move_down(1),
            KeyCode::Char('k') | KeyCode::Up => self.pane_mut().move_up(1),
            KeyCode::Char('l') | KeyCode::Right => self.pane_mut().move_right(),
            KeyCode::Char('w') if !ctrl => self.pane_mut().move_word_forward(),
            KeyCode::Char('b') if !ctrl => self.pane_mut().move_word_backward(),
            KeyCode::Char('0') | KeyCode::Home => self.pane_mut().move_line_start(),
            KeyCode::Char('$') | KeyCode::End => self.pane_mut().move_line_end(),
            KeyCode::Char('G') => self.pane_mut().move_file_end(),

            // Ctrl scrolling
            KeyCode::Char('d') if ctrl => {
                self.pane_mut().move_down(20);
            }
            KeyCode::Char('u') if ctrl => {
                self.pane_mut().move_up(20);
            }
            KeyCode::Char('f') if ctrl => {
                self.pane_mut().move_down(40);
            }
            KeyCode::Char('b') if ctrl => {
                self.pane_mut().move_up(40);
            }

            // ── Undo ─────────────────────────────────────────────────────────
            KeyCode::Char('u') => {
                if !self.pane_mut().undo() {
                    self.message = Some("Already at oldest change".into());
                }
            }

            // ── Delete ───────────────────────────────────────────────────────
            KeyCode::Char('x') => self.pane_mut().delete_char_at_cursor(),

            // ── Paste ────────────────────────────────────────────────────────
            KeyCode::Char('p') => {
                let reg = self.register.clone();
                self.pane_mut().paste_lines_after(&reg);
            }
            KeyCode::Char('P') => {
                let reg = self.register.clone();
                self.pane_mut().paste_lines_before(&reg);
            }

            // ── Mode switches ─────────────────────────────────────────────────
            KeyCode::Char('i') => {
                self.mode = Mode::Insert;
            }
            KeyCode::Char('a') => {
                let col = self.pane().cursor_col + 1;
                let max = self.pane().current_line_len();
                self.pane_mut().cursor_col = col.min(max);
                self.mode = Mode::Insert;
            }
            KeyCode::Char('A') => {
                let max = self.pane().current_line_len();
                self.pane_mut().cursor_col = max;
                self.mode = Mode::Insert;
            }
            KeyCode::Char('o') => {
                self.pane_mut().open_line_below();
                self.mode = Mode::Insert;
            }
            KeyCode::Char('O') => {
                self.pane_mut().open_line_above();
                self.mode = Mode::Insert;
            }
            KeyCode::Char('v') => {
                let (r, c) = (self.pane().cursor_row, self.pane().cursor_col);
                self.visual_anchor = Some((r, c));
                self.mode = Mode::Visual { line_wise: false };
            }
            KeyCode::Char('V') => {
                let r = self.pane().cursor_row;
                self.visual_anchor = Some((r, 0));
                self.mode = Mode::Visual { line_wise: true };
            }
            KeyCode::Char(':') => {
                self.command_buf.clear();
                self.mode = Mode::Command;
            }
            KeyCode::Char('/') => {
                let (r, c) = (self.pane().cursor_row, self.pane().cursor_col);
                self.search_origin = (r, c);
                self.search_buf.clear();
                self.search_matches.clear();
                self.mode = Mode::Search;
            }
            KeyCode::Char('n') => self.next_search_match(),
            KeyCode::Char('N') => self.prev_search_match(),

            // ── Org-mode ──────────────────────────────────────────────────────
            KeyCode::Enter => {
                if let Some(url) = self.pane().link_at_cursor() {
                    if is_web_url(&url) {
                        open_url(&url);
                    } else {
                        let raw = url.strip_prefix("file:").unwrap_or(&url);
                        let resolved = self.resolve_link_path(raw);
                        if resolved.is_dir() {
                            self.enter_browse(&resolved);
                        } else if let Some(path) = resolved.to_str() {
                            self.open_file(path, false);
                        }
                    }
                } else {
                    self.pane_mut().toggle_checkbox();
                }
            }

            // ── Misc ──────────────────────────────────────────────────────────
            KeyCode::Esc => {
                self.key_seq.clear();
                self.search_matches.clear();
            }
            _ => {}
        }
    }

    // ── Action dispatch ───────────────────────────────────────────────────────

    fn dispatch_action(&mut self, action: Action) {
        match action {
            Action::GoToFileStart => self.pane_mut().move_file_start(),
            Action::GoToFileEnd => self.pane_mut().move_file_end(),
            Action::DeleteLine => {
                let line = self.pane_mut().delete_line();
                self.register = vec![line];
            }
            Action::DeleteWord => self.pane_mut().delete_word(),
            Action::DeleteChar => self.pane_mut().delete_char_at_cursor(),
            Action::YankLine => {
                let line = self.pane().yank_line();
                self.register = vec![line];
                self.message = Some("1 line yanked".into());
            }
            Action::SplitHorizontal => self.split(SplitDir::Horizontal),
            Action::SplitVertical => self.split(SplitDir::Vertical),
            Action::ClosePane => self.close_active_pane(),
            Action::NextPane => self.cycle_pane(),
            Action::FocusPane(dir) => self.focus_pane(dir),
            Action::SaveFile => self.save_active(),
            Action::ToggleLinkConceal => {
                self.conceal_links = !self.conceal_links;
                self.message = Some(if self.conceal_links {
                    "Links: showing label".into()
                } else {
                    "Links: showing target".into()
                });
            }
            Action::QuitAll => {
                if self.panes.iter().any(|p| p.modified) {
                    self.message = Some("Unsaved changes — use :q! or :wq".into());
                } else {
                    self.should_quit = true;
                }
            }
            Action::BufferList => self.enter_buffer_list(),
            Action::NextBuffer => self.cycle_buffer(1),
            Action::PrevBuffer => self.cycle_buffer(-1),
            Action::KillBuffer => self.kill_current_buffer(),
        }
    }

    // ── Insert mode ───────────────────────────────────────────────────────────

    fn handle_insert(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if self.pane().cursor_col > 0 {
                    self.pane_mut().cursor_col -= 1;
                }
                self.mode = Mode::Normal;
            }
            KeyCode::Char(c) => self.pane_mut().insert_char(c),
            KeyCode::Backspace => self.pane_mut().delete_char_before(),
            KeyCode::Enter => self.pane_mut().insert_newline(),
            KeyCode::Left => self.pane_mut().move_left(),
            KeyCode::Right => self.pane_mut().move_right(),
            KeyCode::Up => self.pane_mut().move_up(1),
            KeyCode::Down => self.pane_mut().move_down(1),
            _ => {}
        }
    }

    // ── Command mode ──────────────────────────────────────────────────────────

    fn handle_command(&mut self, key: KeyEvent) {
        // Any key other than Tab clears an active completion session.
        if key.code != KeyCode::Tab {
            self.completions = None;
        }

        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.command_buf.clear();
            }
            KeyCode::Enter => {
                let cmd = std::mem::take(&mut self.command_buf);
                self.execute_command(&cmd);
                if self.mode == Mode::Command {
                    self.mode = Mode::Normal;
                }
            }
            KeyCode::Backspace => {
                if self.command_buf.pop().is_none() {
                    self.mode = Mode::Normal;
                }
            }
            KeyCode::Tab => {
                self.tab_complete();
            }
            KeyCode::Char(c) => {
                self.command_buf.push(c);
            }
            _ => {}
        }
    }

    /// Cycle through filesystem completions for the current command buffer.
    fn tab_complete(&mut self) {
        if let Some(ref mut state) = self.completions {
            // Already have a completion session — advance to next candidate.
            if state.candidates.is_empty() {
                return;
            }
            state.idx = (state.idx + 1) % state.candidates.len();
            let candidate = state.candidates[state.idx].clone();
            self.command_buf = format!("{}{}", state.cmd_prefix, candidate);
            self.show_completion_hint();
        } else {
            // Start a new completion session.
            let Some((cmd_prefix, partial)) = split_file_command(&self.command_buf) else {
                return;
            };
            let candidates = path_completions(&partial);
            if candidates.is_empty() {
                self.message = Some("No completions".into());
                return;
            }
            // Insert the first candidate immediately.
            self.command_buf = format!("{}{}", cmd_prefix, candidates[0]);
            self.completions = Some(CompletionState {
                cmd_prefix,
                candidates,
                idx: 0,
            });
            self.show_completion_hint();
        }
    }

    fn show_completion_hint(&mut self) {
        if let Some(ref state) = self.completions {
            if state.candidates.len() == 1 {
                self.message = None;
            } else {
                let list = state.candidates.join("  ");
                self.message = Some(list);
            }
        }
    }

    fn execute_command(&mut self, cmd: &str) {
        let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
        match parts[0] {
            "q" => {
                if self.panes.iter().any(|p| p.modified) {
                    self.message = Some("Unsaved changes — use :q! or :wq".into());
                } else {
                    self.should_quit = true;
                }
            }
            "q!" => self.should_quit = true,
            "w" => self.save_active(),
            "wq" | "x" => {
                self.save_active();
                self.should_quit = true;
            }
            "sp" | "split" => self.split(SplitDir::Horizontal),
            "vs" | "vsplit" => self.split(SplitDir::Vertical),
            "e" | "edit" => {
                if let Some(path) = parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                    if Path::new(path).is_dir() {
                        self.enter_browse(Path::new(path));
                    } else {
                        self.open_file(path, false);
                    }
                } else {
                    self.message = Some("Usage: :e <filename>".into());
                }
            }
            "e!" | "edit!" => {
                if let Some(path) = parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                    if Path::new(path).is_dir() {
                        self.enter_browse(Path::new(path));
                    } else {
                        self.open_file(path, true);
                    }
                } else {
                    self.message = Some("Usage: :e! <filename>".into());
                }
            }
            "Ex" | "Explore" | "explore" => {
                let dir = match parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                    Some(path) => StdPathBuf::from(path),
                    None => self
                        .pane()
                        .file_path
                        .as_ref()
                        .and_then(|p| p.parent())
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| StdPathBuf::from(".")),
                };
                self.enter_browse(&dir);
            }
            other => {
                self.message = Some(format!("Unknown command: {}", other));
            }
        }
    }

    // ── Search mode ───────────────────────────────────────────────────────────

    fn handle_search(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                let (r, c) = self.search_origin;
                self.pane_mut().cursor_row = r;
                self.pane_mut().cursor_col = c;
                self.search_matches.clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                if self.search_matches.is_empty() && !self.search_buf.is_empty() {
                    self.message = Some(format!("Pattern not found: {}", self.search_buf));
                }
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                self.search_buf.pop();
                self.recompute_search(true);
            }
            KeyCode::Char(c) => {
                self.search_buf.push(c);
                self.recompute_search(false);
            }
            _ => {}
        }
    }

    /// Recompute `search_matches` from the current `search_buf` and jump to
    /// the nearest match at or after `search_origin`.  When `from_origin` is
    /// true the match index resets from the origin even if the query shrank.
    fn recompute_search(&mut self, from_origin: bool) {
        let query = self.search_buf.clone();
        if query.is_empty() {
            self.search_matches.clear();
            let (r, c) = self.search_origin;
            self.pane_mut().cursor_row = r;
            self.pane_mut().cursor_col = c;
            return;
        }
        let qlen = query.len();
        self.search_matches = self
            .pane()
            .lines
            .iter()
            .enumerate()
            .flat_map(|(row, line)| {
                let mut hits = Vec::new();
                let mut start = 0;
                while let Some(pos) = line[start..].find(query.as_str()) {
                    hits.push((row, start + pos));
                    start += pos + qlen.max(1);
                }
                hits
            })
            .collect();

        let (or, oc) = self.search_origin;
        if self.search_matches.is_empty() {
            return;
        }
        if from_origin || self.search_match_idx >= self.search_matches.len() {
            self.search_match_idx = self
                .search_matches
                .iter()
                .position(|&(r, c)| r > or || (r == or && c >= oc))
                .unwrap_or(0);
        }
        self.jump_to_current_match();
    }

    fn jump_to_current_match(&mut self) {
        if let Some(&(row, col)) = self.search_matches.get(self.search_match_idx) {
            let pane = self.pane_mut();
            pane.cursor_row = row;
            pane.cursor_col = col;
        }
    }

    fn next_search_match(&mut self) {
        if self.search_matches.is_empty() {
            if !self.search_buf.is_empty() {
                self.message = Some(format!("Pattern not found: {}", self.search_buf));
            }
            return;
        }
        self.search_match_idx = (self.search_match_idx + 1) % self.search_matches.len();
        self.jump_to_current_match();
    }

    fn prev_search_match(&mut self) {
        if self.search_matches.is_empty() {
            if !self.search_buf.is_empty() {
                self.message = Some(format!("Pattern not found: {}", self.search_buf));
            }
            return;
        }
        self.search_match_idx = self
            .search_match_idx
            .checked_sub(1)
            .unwrap_or(self.search_matches.len() - 1);
        self.jump_to_current_match();
    }

    // ── Visual mode ───────────────────────────────────────────────────────────

    fn handle_visual(&mut self, key: KeyEvent, line_wise: bool) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.visual_anchor = None;
            }
            // Movement — extends selection
            KeyCode::Char('h') | KeyCode::Left => self.pane_mut().move_left(),
            KeyCode::Char('j') | KeyCode::Down => self.pane_mut().move_down(1),
            KeyCode::Char('k') | KeyCode::Up => self.pane_mut().move_up(1),
            KeyCode::Char('l') | KeyCode::Right => self.pane_mut().move_right(),
            KeyCode::Char('w') => self.pane_mut().move_word_forward(),
            KeyCode::Char('b') => self.pane_mut().move_word_backward(),
            KeyCode::Char('0') | KeyCode::Home => self.pane_mut().move_line_start(),
            KeyCode::Char('$') | KeyCode::End => self.pane_mut().move_line_end(),
            KeyCode::Char('G') => self.pane_mut().move_file_end(),
            KeyCode::Char('g') => {
                // gg in visual (single keypress is fine here — no ambiguity needed)
                self.pane_mut().move_file_start();
            }
            // Operators on selection
            KeyCode::Char('d') | KeyCode::Char('x') => {
                let anchor = self
                    .visual_anchor
                    .unwrap_or((self.pane().cursor_row, self.pane().cursor_col));
                let cursor = (self.pane().cursor_row, self.pane().cursor_col);
                if line_wise {
                    let removed = self.pane_mut().delete_lines(anchor.0, cursor.0);
                    self.register = removed;
                } else {
                    let removed = self.pane_mut().delete_char_selection(anchor, cursor);
                    self.register = removed;
                }
                self.mode = Mode::Normal;
                self.visual_anchor = None;
            }
            KeyCode::Char('y') => {
                let anchor = self
                    .visual_anchor
                    .unwrap_or((self.pane().cursor_row, self.pane().cursor_col));
                let cursor_row = self.pane().cursor_row;
                if line_wise {
                    self.register = self.pane().yank_lines(anchor.0, cursor_row);
                } else {
                    // Char-wise yank: collect the text (simplified to full lines)
                    self.register = self.pane().yank_lines(anchor.0, cursor_row);
                }
                self.message = Some(format!("{} lines yanked", self.register.len()));
                self.mode = Mode::Normal;
                self.visual_anchor = None;
            }
            // Toggle between V and v
            KeyCode::Char('v') if line_wise => {
                let (r, c) = (self.pane().cursor_row, self.pane().cursor_col);
                self.visual_anchor = Some((r, c));
                self.mode = Mode::Visual { line_wise: false };
            }
            KeyCode::Char('V') if !line_wise => {
                let r = self.pane().cursor_row;
                self.visual_anchor = Some((r, 0));
                self.mode = Mode::Visual { line_wise: true };
            }
            _ => {}
        }
    }

    // ── Window management ─────────────────────────────────────────────────────

    fn split(&mut self, dir: SplitDir) {
        let new_pane = match &self.panes[self.active_pane].file_path {
            Some(p) => Pane::from_file(p.to_str().unwrap_or("")).unwrap_or_else(|_| Pane::empty()),
            None => Pane::empty(),
        };
        let new_idx = self.panes.len();
        self.panes.push(new_pane);
        self.layout.split_leaf(self.active_pane, new_idx, dir);
        self.active_pane = new_idx; // focus the new pane
    }

    fn close_active_pane(&mut self) {
        if self.panes.len() == 1 {
            // Last pane — quit
            if self.panes[0].modified {
                self.message = Some("Unsaved changes — :q! or :wq".into());
            } else {
                self.should_quit = true;
            }
            return;
        }
        let closed = self.active_pane;
        self.layout.remove_leaf(closed);
        self.layout.renumber_after_removal(closed);
        self.panes.remove(closed);
        self.active_pane = closed.min(self.panes.len() - 1);
    }

    fn cycle_pane(&mut self) {
        let leaves = self.layout.leaves();
        if leaves.len() > 1 {
            if let Some(pos) = leaves.iter().position(|&i| i == self.active_pane) {
                self.active_pane = leaves[(pos + 1) % leaves.len()];
            }
        }
    }

    /// Move focus to the geometrically nearest pane in `dir`, using the pane
    /// rects computed by the last render pass. Works for any tree shape
    /// (nested hsplits/vsplits), not just a simple 2-pane layout.
    fn focus_pane(&mut self, dir: PaneDir) {
        if self.panes.len() < 2 || self.pane_rects.len() != self.panes.len() {
            return;
        }
        let cur = self.pane_rects[self.active_pane];
        let cur_center_x = cur.x as i32 + cur.width as i32 / 2;
        let cur_center_y = cur.y as i32 + cur.height as i32 / 2;

        let mut best: Option<(usize, i32)> = None;
        for (i, rect) in self.pane_rects.iter().enumerate() {
            if i == self.active_pane {
                continue;
            }
            let in_direction = match dir {
                PaneDir::Left => rect.x as i32 + rect.width as i32 <= cur.x as i32,
                PaneDir::Right => rect.x as i32 >= cur.x as i32 + cur.width as i32,
                PaneDir::Up => rect.y as i32 + rect.height as i32 <= cur.y as i32,
                PaneDir::Down => rect.y as i32 >= cur.y as i32 + cur.height as i32,
            };
            if !in_direction {
                continue;
            }
            let center_x = rect.x as i32 + rect.width as i32 / 2;
            let center_y = rect.y as i32 + rect.height as i32 / 2;
            // Primary-axis gap dominates; cross-axis offset breaks ties so the
            // best-aligned neighbor wins over a merely closer one.
            let dist = match dir {
                PaneDir::Left | PaneDir::Right => {
                    (center_x - cur_center_x).abs() + (center_y - cur_center_y).abs() * 4
                }
                PaneDir::Up | PaneDir::Down => {
                    (center_y - cur_center_y).abs() + (center_x - cur_center_x).abs() * 4
                }
            };
            if best.is_none_or(|(_, best_dist)| dist < best_dist) {
                best = Some((i, dist));
            }
        }
        if let Some((idx, _)) = best {
            self.active_pane = idx;
        }
    }

    // ── File I/O ──────────────────────────────────────────────────────────────

    /// Resolve a link target against the active pane's file directory, so
    /// relative links (e.g. `[[notes/foo.org]]`) work regardless of the
    /// process's current working directory.
    fn resolve_link_path(&self, raw: &str) -> StdPathBuf {
        let p = Path::new(raw);
        if p.is_absolute() {
            return p.to_path_buf();
        }
        match self.pane().file_path.as_ref().and_then(|f| f.parent()) {
            Some(dir) if !dir.as_os_str().is_empty() => dir.join(p),
            _ => p.to_path_buf(),
        }
    }

    fn open_file(&mut self, path: &str, force: bool) {
        if !force && self.pane().modified {
            self.message = Some("Unsaved changes — :w first, or :e! to discard".into());
            return;
        }
        match Pane::from_file(path) {
            Ok(pane) => {
                if let Some(p) = pane.file_path.clone() {
                    self.record_buffer(p);
                }
                self.panes[self.active_pane] = pane;
                self.visual_anchor = None;
                self.key_seq.clear();
                self.mode = Mode::Normal;
            }
            Err(e) => {
                self.message = Some(format!("Cannot open \"{}\": {}", path, e));
            }
        }
    }

    // ── Buffer list ───────────────────────────────────────────────────────────

    /// Move `path` to the front of the MRU buffer list, deduping.
    fn record_buffer(&mut self, path: StdPathBuf) {
        self.buffers.retain(|p| p != &path);
        self.buffers.insert(0, path);
    }

    fn enter_buffer_list(&mut self) {
        self.buf_list_selected = self
            .pane()
            .file_path
            .as_ref()
            .and_then(|active| self.buffers.iter().position(|p| p == active))
            .unwrap_or(0);
        self.buf_list_scroll = 0;
        self.mode = Mode::BufferList;
        self.key_seq.clear();
    }

    fn handle_buffer_list(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.buffers.is_empty() {
                    self.buf_list_selected =
                        (self.buf_list_selected + 1).min(self.buffers.len() - 1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.buf_list_selected = self.buf_list_selected.saturating_sub(1);
            }
            KeyCode::Char('d') => {
                if self.buf_list_selected < self.buffers.len() {
                    self.buffers.remove(self.buf_list_selected);
                    if self.buf_list_selected >= self.buffers.len() {
                        self.buf_list_selected = self.buffers.len().saturating_sub(1);
                    }
                }
            }
            KeyCode::Enter => {
                let Some(path) = self.buffers.get(self.buf_list_selected).cloned() else {
                    self.mode = Mode::Normal;
                    return;
                };
                if let Some(path_str) = path.to_str().map(|s| s.to_string()) {
                    self.open_file(&path_str, false);
                }
            }
            _ => {}
        }
    }

    /// Switch the active pane to the next/previous buffer in the MRU list
    /// (`dir` = +1 or -1), relative to the pane's current file.
    fn cycle_buffer(&mut self, dir: isize) {
        if self.buffers.len() < 2 {
            self.message = Some("No other buffers".into());
            return;
        }
        let current = self
            .pane()
            .file_path
            .as_ref()
            .and_then(|active| self.buffers.iter().position(|p| p == active));
        let len = self.buffers.len() as isize;
        let next_idx = match current {
            Some(idx) => (idx as isize + dir).rem_euclid(len) as usize,
            None => 0,
        };
        let Some(path_str) = self.buffers[next_idx].to_str().map(|s| s.to_string()) else {
            return;
        };
        self.open_file(&path_str, false);
    }

    /// Forget the active pane's file from the buffer list and switch the
    /// pane to the next most-recent buffer (or an empty buffer if none left).
    fn kill_current_buffer(&mut self) {
        if self.pane().modified {
            self.message = Some("Unsaved changes — save first with :w".into());
            return;
        }
        let Some(path) = self.pane().file_path.clone() else {
            self.message = Some("No file buffer to kill".into());
            return;
        };
        self.buffers.retain(|p| p != &path);
        match self.buffers.first().cloned() {
            Some(next) => {
                if let Some(path_str) = next.to_str().map(|s| s.to_string()) {
                    self.open_file(&path_str, false);
                }
            }
            None => {
                self.panes[self.active_pane] = Pane::empty();
            }
        }
        self.message = Some(format!("Killed buffer {}", path.display()));
    }

    // ── Directory browser ─────────────────────────────────────────────────────

    /// List `dir` and switch to `Mode::Browse`. Shows subdirectories and
    /// `.org` files only, directories first, alphabetically within each group.
    fn enter_browse(&mut self, dir: &Path) {
        let mut entries: Vec<BrowseEntry> = match std::fs::read_dir(dir) {
            Ok(read_dir) => read_dir
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name().into_string().ok()?;
                    if name.starts_with('.') {
                        return None;
                    }
                    let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    if !is_dir && !name.ends_with(".org") {
                        return None;
                    }
                    Some(BrowseEntry { name, is_dir })
                })
                .collect(),
            Err(e) => {
                self.message = Some(format!("Cannot open \"{}\": {}", dir.display(), e));
                return;
            }
        };
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
        if dir.parent().is_some() {
            entries.insert(
                0,
                BrowseEntry {
                    name: "..".to_string(),
                    is_dir: true,
                },
            );
        }
        self.browse_dir = dir.to_path_buf();
        self.browse_entries = entries;
        self.browse_selected = 0;
        self.browse_scroll = 0;
        self.mode = Mode::Browse;
        self.key_seq.clear();
    }

    fn handle_browse(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.browse_entries.is_empty() {
                    self.browse_selected =
                        (self.browse_selected + 1).min(self.browse_entries.len() - 1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.browse_selected = self.browse_selected.saturating_sub(1);
            }
            KeyCode::Char('d') if ctrl => {
                if !self.browse_entries.is_empty() {
                    self.browse_selected =
                        (self.browse_selected + 20).min(self.browse_entries.len() - 1);
                }
            }
            KeyCode::Char('u') if ctrl => {
                self.browse_selected = self.browse_selected.saturating_sub(20);
            }
            KeyCode::Char('-') | KeyCode::Backspace => {
                if let Some(parent) = self.browse_dir.clone().parent() {
                    self.enter_browse(parent);
                }
            }
            KeyCode::Enter => {
                let Some(entry) = self.browse_entries.get(self.browse_selected).cloned() else {
                    return;
                };
                if entry.name == ".." {
                    if let Some(parent) = self.browse_dir.clone().parent() {
                        self.enter_browse(parent);
                    }
                    return;
                }
                let target = self.browse_dir.join(&entry.name);
                if entry.is_dir {
                    self.enter_browse(&target);
                } else if let Some(path) = target.to_str() {
                    self.open_file(path, false);
                }
            }
            _ => {}
        }
    }

    fn save_active(&mut self) {
        match self.panes[self.active_pane].save() {
            Ok(msg) => self.message = Some(msg),
            Err(e) => self.message = Some(e),
        }
    }

    // ── Visual selection query (used by ui.rs) ────────────────────────────────

    /// Returns `Some((start_row, start_col, end_row, end_col, line_wise))` when
    /// a visual selection is active, with start ≤ end guaranteed.
    pub fn visual_selection(&self) -> Option<(usize, usize, usize, usize, bool)> {
        let line_wise = match self.mode {
            Mode::Visual { line_wise } => line_wise,
            _ => return None,
        };
        let (ar, ac) = self.visual_anchor?;
        let pane = self.pane();
        let (cr, cc) = (pane.cursor_row, pane.cursor_col);
        let ((sr, sc), (er, ec)) = if (ar, ac) <= (cr, cc) {
            ((ar, ac), (cr, cc))
        } else {
            ((cr, cc), (ar, ac))
        };
        Some((sr, sc, er, ec, line_wise))
    }
}
