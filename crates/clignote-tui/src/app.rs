use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::config::Config;
use crate::journal;
use crate::keymap::{self, Action, MatchResult, PaneDir};
use crate::pane::Pane;
use crate::structure::StructureOp;

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
    let (search_dir, name_prefix): (PathBuf, &str) = if partial.ends_with('/') {
        (PathBuf::from(partial), "")
    } else {
        let p = Path::new(partial);
        let dir = match p.parent() {
            Some(d) if d != Path::new("") => d.to_path_buf(),
            _ => PathBuf::from("."),
        };
        (dir, p.file_name().and_then(|n| n.to_str()).unwrap_or(""))
    };
    let bare_cwd = search_dir == Path::new(".") && !partial.starts_with("./");
    // Avoid a doubled slash when `partial` already ended in one.
    let dir_prefix = search_dir.to_string_lossy();
    let dir_prefix = dir_prefix.trim_end_matches('/');

    let mut results: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&search_dir) {
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            // Skip hidden entries unless the user is explicitly typing a dot
            if name.starts_with('.') && !name_prefix.starts_with('.') {
                continue;
            }
            if !name.starts_with(name_prefix) {
                continue;
            }
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            let slash = if is_dir { "/" } else { "" };
            // Reconstruct the full path string to return
            results.push(if bare_cwd {
                format!("{name}{slash}")
            } else {
                format!("{dir_prefix}/{name}{slash}")
            });
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
                return Some((format!("{verb} "), rest.trim().to_string()));
            }
        }
    }
    None
}

// ── Mode ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub fn label(self) -> &'static str {
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

/// How the line-number column is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineNumbers {
    #[default]
    Off,
    Absolute,
    /// Distance from the cursor row; the cursor row shows its absolute number.
    Relative,
}

impl LineNumbers {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Absolute,
            Self::Absolute => Self::Relative,
            Self::Relative => Self::Off,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "off" => Some(Self::Off),
            "absolute" => Some(Self::Absolute),
            "relative" => Some(Self::Relative),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Absolute => "absolute",
            Self::Relative => "relative",
        }
    }
}

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
    /// Commit-and-push running on a worker thread (`SPC f S`).
    pub commit_job: Option<CommitJob>,

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

    /// When true (default), long lines wrap at word boundaries and `j`/`k`
    /// move by screen row; when false they scroll horizontally.
    /// Toggle with `SPC t w`.
    pub wrap: bool,

    /// When true (default), body text under a heading is shown indented by
    /// the heading's level. Display only; the buffer is untouched.
    /// Toggle with `SPC t i`.
    pub indent: bool,

    /// Line-number column: off (default), absolute, or relative to the cursor.
    /// Cycle with `SPC t n`.
    pub line_numbers: LineNumbers,

    // Directory browser state (Mode::Browse)
    pub browse_dir: PathBuf,
    pub browse_entries: Vec<BrowseEntry>,
    pub browse_selected: usize,
    /// First visible entry index (vertical scroll), kept in sync by the renderer.
    pub browse_scroll: usize,

    /// Most-recently-used list of file paths opened this session (front = most recent).
    pub buffers: Vec<PathBuf>,
    /// Selection state for Mode::BufferList.
    pub buf_list_selected: usize,
    pub buf_list_scroll: usize,
    /// User configuration (file + environment).
    config: Config,
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

/// Move a list selection down by `n`, clamped to the last of `len` entries.
fn select_down(selected: &mut usize, len: usize, n: usize) {
    *selected = (*selected + n).min(len.saturating_sub(1));
}

impl App {
    pub fn new(file_path: Option<&str>) -> anyhow::Result<Self> {
        let browse_target = match file_path {
            Some(p) if Path::new(p).is_dir() => Some(PathBuf::from(p)),
            _ => None,
        };
        let pane = match file_path {
            Some(p) if browse_target.is_none() => Pane::from_file(p)?,
            _ => Pane::empty(),
        };
        let buffers = match &pane.file_path {
            Some(p) => vec![p.clone()],
            None => Vec::new(),
        };
        let (config, config_warning) = Config::load();
        let mut app = Self {
            mode: Mode::Normal,
            panes: vec![pane],
            active_pane: 0,
            layout: PaneLayout::Leaf(0),
            register: Vec::new(),
            visual_anchor: None,
            key_seq: Vec::new(),
            command_buf: String::new(),
            message: None,
            commit_job: None,
            should_quit: false,
            pane_rects: Vec::new(),
            completions: None,
            search_buf: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
            search_origin: (0, 0),
            conceal_links: true,
            wrap: true,
            indent: true,
            line_numbers: config.line_numbers,
            browse_dir: PathBuf::new(),
            browse_entries: Vec::new(),
            browse_selected: 0,
            browse_scroll: 0,
            buffers,
            buf_list_selected: 0,
            buf_list_scroll: 0,
            config,
        };
        app.message = config_warning;
        if let Some(dir) = browse_target {
            app.enter_browse(&dir);
        }
        Ok(app)
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
        match self.mode {
            Mode::Normal => self.handle_normal(key),
            Mode::Insert => self.handle_insert(key),
            Mode::Command => self.handle_command(key),
            Mode::Search => self.handle_search(key),
            Mode::Visual { line_wise } => self.handle_visual(key, line_wise),
            Mode::Browse => self.handle_browse(key),
            Mode::BufferList => self.handle_buffer_list(key),
        }
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.click_at(event.column as usize, event.row as usize);
            }
            MouseEventKind::ScrollDown => self.pane_mut().scroll_down(3),
            MouseEventKind::ScrollUp => self.pane_mut().scroll_up(3),
            _ => {}
        }
    }

    fn click_at(&mut self, col: usize, row: usize) {
        // Find which pane the click landed in
        let hit = self
            .pane_rects
            .iter()
            .position(|r| r.contains(Position::new(col as u16, row as u16)));
        let Some(i) = hit else {
            return;
        };
        let rect = self.pane_rects[i];
        self.active_pane = i;
        let pane = &mut self.panes[i];
        // Content starts after the gutter (git mark + line numbers).
        let gutter = crate::ui::gutter_width(self.line_numbers, pane.lines.len(), rect.width);
        let content_x = rect.x as usize + gutter as usize;
        let (buf_row, buf_col) =
            pane.pos_at_screen(col.saturating_sub(content_x), row - rect.y as usize);
        pane.cursor_row = buf_row;
        pane.cursor_col = buf_col;
        // Leave visual mode on click
        if matches!(self.mode, Mode::Visual { .. }) {
            self.exit_visual();
        }
    }

    // ── Normal mode ───────────────────────────────────────────────────────────

    fn handle_normal(&mut self, key: KeyEvent) {
        // C-n / C-p walk journal days, but only inside a journal file.
        if self.key_seq.is_empty() && key.modifiers == KeyModifiers::CONTROL {
            let forward = match key.code {
                KeyCode::Char('n') => Some(true),
                KeyCode::Char('p') => Some(false),
                _ => None,
            };
            if let Some(forward) = forward.filter(|_| self.current_journal_date().is_some()) {
                self.journal_step(forward);
                return;
            }
        }
        self.key_seq.push(key);
        let seq_str = keymap::seq_to_str(&self.key_seq);

        match keymap::match_seq(&seq_str) {
            MatchResult::Prefix => {
                let hint = keymap::hint_for_prefix(&seq_str);
                self.message = Some(format!("[{seq_str}]  {hint}"));
                // keep accumulating
            }
            MatchResult::Action(action) => {
                self.key_seq.clear();
                self.dispatch_action(action);
            }
            MatchResult::NoMatch => {
                // A failed multi-key sequence is discarded; only a lone key
                // falls through to the single-key bindings.
                let solo = (self.key_seq.len() == 1).then(|| self.key_seq[0]);
                self.key_seq.clear();
                if let Some(key) = solo {
                    self.handle_single_normal(key);
                }
            }
        }
    }

    /// Cursor motions shared by Normal and Visual mode. Returns whether `key`
    /// was a motion (and has been applied).
    fn apply_motion(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let pane = self.pane_mut();
        match key.code {
            KeyCode::Char('h') | KeyCode::Left => pane.move_left(),
            KeyCode::Char('j') | KeyCode::Down => pane.move_down(1),
            KeyCode::Char('k') | KeyCode::Up => pane.move_up(1),
            KeyCode::Char('l') | KeyCode::Right => pane.move_right(),
            KeyCode::Char('w') if !ctrl => pane.move_word_forward(),
            KeyCode::Char('b') if !ctrl => pane.move_word_backward(),
            KeyCode::Char('0') | KeyCode::Home => pane.move_line_start(),
            KeyCode::Char('$') | KeyCode::End => pane.move_line_end(),
            KeyCode::Char('G') => pane.move_file_end(),
            _ => return false,
        }
        true
    }

    /// Run an org structural command and report the outcome in the cmdline.
    fn structure_edit(&mut self, op: StructureOp) {
        let result = self.pane_mut().apply_structure(op);
        self.message = Some(result.unwrap_or_else(|e| e).to_string());
    }

    /// `M-h/l/j/k` (and `M-←/→`) edit headings, `M-H/L` whole subtrees,
    /// `S-←/→` cycle the TODO keyword. Returns whether `key` was one of these.
    fn structure_key(&mut self, key: KeyEvent) -> bool {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let op = match key.code {
            KeyCode::Char('h') | KeyCode::Left if alt => StructureOp::Promote,
            KeyCode::Char('l') | KeyCode::Right if alt => StructureOp::Demote,
            KeyCode::Char('H') if alt => StructureOp::PromoteSubtree,
            KeyCode::Char('L') if alt => StructureOp::DemoteSubtree,
            KeyCode::Char('k') | KeyCode::Up if alt => StructureOp::MoveUp,
            KeyCode::Char('j') | KeyCode::Down if alt => StructureOp::MoveDown,
            KeyCode::Right if shift => StructureOp::TodoNext,
            KeyCode::Left if shift => StructureOp::TodoPrev,
            _ => return false,
        };
        self.structure_edit(op);
        true
    }

    /// Handle single-key normal-mode bindings (not part of a multi-key sequence).
    fn handle_single_normal(&mut self, key: KeyEvent) {
        if self.structure_key(key) {
            return;
        }
        if self.apply_motion(key) {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // Ctrl scrolling
            KeyCode::Char('d') if ctrl => self.pane_mut().move_down(20),
            KeyCode::Char('u') if ctrl => self.pane_mut().move_up(20),
            KeyCode::Char('f') if ctrl => self.pane_mut().move_down(40),
            KeyCode::Char('b') if ctrl => self.pane_mut().move_up(40),

            // ── Undo ─────────────────────────────────────────────────────────
            KeyCode::Char('u') => {
                if !self.pane_mut().undo() {
                    self.message = Some("Already at oldest change".into());
                }
            }
            KeyCode::Char('r') if ctrl => {
                if !self.pane_mut().redo() {
                    self.message = Some("Already at newest change".into());
                }
            }

            // ── Delete ───────────────────────────────────────────────────────
            KeyCode::Char('x') => self.pane_mut().delete_char_at_cursor(),

            // ── Paste ────────────────────────────────────────────────────────
            KeyCode::Char('p') => self.panes[self.active_pane].paste_lines_after(&self.register),
            KeyCode::Char('P') => self.panes[self.active_pane].paste_lines_before(&self.register),

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
            KeyCode::Char('v') => self.enter_visual(false),
            KeyCode::Char('V') => self.enter_visual(true),
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
            KeyCode::Tab => self.message = Some(self.pane_mut().cycle_fold()),
            KeyCode::BackTab => self.message = Some(self.pane_mut().cycle_global_fold()),
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
                        } else {
                            self.open_file(&resolved, false);
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
            Action::DeleteLine => {
                let line = self.pane_mut().delete_line();
                self.register = vec![line];
            }
            Action::DeleteWord => self.pane_mut().delete_word(),
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
            Action::SaveCommitPush => self.save_commit_push(),
            Action::ToggleWrap => {
                self.wrap = !self.wrap;
                self.message = Some(if self.wrap {
                    "Soft wrap: on".into()
                } else {
                    "Soft wrap: off".into()
                });
            }
            Action::ToggleIndent => {
                self.indent = !self.indent;
                self.message = Some(if self.indent {
                    "Indent: on".into()
                } else {
                    "Indent: off".into()
                });
            }
            Action::ToggleLineNumbers => {
                self.line_numbers = self.line_numbers.next();
                self.message = Some(format!("Line numbers: {}", self.line_numbers.label()));
            }
            Action::ToggleLinkConceal => {
                self.conceal_links = !self.conceal_links;
                self.message = Some(if self.conceal_links {
                    "Links: showing label".into()
                } else {
                    "Links: showing target".into()
                });
            }
            Action::QuitAll => self.quit_if_clean(),
            Action::BufferList => self.enter_buffer_list(),
            Action::NextBuffer => self.cycle_buffer(1),
            Action::PrevBuffer => self.cycle_buffer(-1),
            Action::KillBuffer => self.kill_current_buffer(),
            Action::JournalNewEntry => self.journal_open_today(true),
            Action::JournalToday => self.journal_open_today(false),
            Action::JournalNext => self.journal_step(true),
            Action::JournalPrev => self.journal_step(false),
            Action::Structure(op) => self.structure_edit(op),
        }
    }

    // ── Insert mode ───────────────────────────────────────────────────────────

    fn handle_insert(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if self.pane().cursor_col > 0 {
                    self.pane_mut().cursor_col -= 1;
                }
                self.pane_mut().end_insert();
                self.mode = Mode::Normal;
            }
            KeyCode::Char(c) => self.pane_mut().insert_char(c),
            KeyCode::Backspace => self.pane_mut().delete_char_before(),
            KeyCode::Enter => self.pane_mut().insert_newline(),
            // Moving the cursor ends the insert session (a new undo step).
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                let pane = self.pane_mut();
                pane.end_insert();
                match key.code {
                    KeyCode::Left => pane.move_left(),
                    KeyCode::Right => pane.move_right(),
                    KeyCode::Up => pane.move_up(1),
                    _ => pane.move_down(1),
                }
            }
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

    fn quit_if_clean(&mut self) {
        if self.panes.iter().any(|p| p.modified) {
            self.message = Some("Unsaved changes — use :q! or :wq".into());
        } else {
            self.should_quit = true;
        }
    }

    /// `:e` / `:e!` — open a file, or browse if `arg` names a directory.
    fn edit_command(&mut self, arg: Option<&str>, force: bool) {
        let Some(path) = arg.map(Path::new) else {
            let bang = if force { "!" } else { "" };
            self.message = Some(format!("Usage: :e{bang} <filename>"));
            return;
        };
        if path.is_dir() {
            self.enter_browse(path);
        } else {
            self.open_file(path, force);
        }
    }

    fn execute_command(&mut self, cmd: &str) {
        let cmd = cmd.trim();
        let (verb, arg) = match cmd.split_once(' ') {
            Some((verb, arg)) => (verb, Some(arg.trim()).filter(|s| !s.is_empty())),
            None => (cmd, None),
        };
        match verb {
            "q" => self.quit_if_clean(),
            "q!" => self.should_quit = true,
            "w" => self.save_active(),
            "wq" | "x" => {
                self.save_active();
                self.should_quit = true;
            }
            "sp" | "split" => self.split(SplitDir::Horizontal),
            "vs" | "vsplit" => self.split(SplitDir::Vertical),
            "e" | "edit" => self.edit_command(arg, false),
            "e!" | "edit!" => self.edit_command(arg, true),
            "Ex" | "Explore" | "explore" => {
                let dir = match arg {
                    Some(path) => PathBuf::from(path),
                    None => self
                        .pane()
                        .file_path
                        .as_deref()
                        .and_then(Path::parent)
                        .map_or_else(|| PathBuf::from("."), Path::to_path_buf),
                };
                self.enter_browse(&dir);
            }
            other => {
                self.message = Some(format!("Unknown command: {other}"));
            }
        }
    }

    // ── Search mode ───────────────────────────────────────────────────────────

    fn handle_search(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.restore_search_origin();
                self.search_matches.clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                self.report_if_no_matches();
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

    fn restore_search_origin(&mut self) {
        let (r, c) = self.search_origin;
        let pane = self.pane_mut();
        pane.cursor_row = r;
        pane.cursor_col = c;
    }

    /// Set the "Pattern not found" message when a non-empty query matched nothing.
    fn report_if_no_matches(&mut self) {
        if self.search_matches.is_empty() && !self.search_buf.is_empty() {
            self.message = Some(format!("Pattern not found: {}", self.search_buf));
        }
    }

    /// Recompute `search_matches` from the current `search_buf` and jump to
    /// the nearest match at or after `search_origin`.  When `from_origin` is
    /// true the match index resets from the origin even if the query shrank.
    fn recompute_search(&mut self, from_origin: bool) {
        if self.search_buf.is_empty() {
            self.search_matches.clear();
            self.restore_search_origin();
            return;
        }
        let query = self.search_buf.as_str();
        self.search_matches = self.panes[self.active_pane]
            .lines
            .iter()
            .enumerate()
            .flat_map(|(row, line)| line.match_indices(query).map(move |(col, _)| (row, col)))
            .collect();

        if self.search_matches.is_empty() {
            return;
        }
        if from_origin || self.search_match_idx >= self.search_matches.len() {
            let (or, oc) = self.search_origin;
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
            pane.reveal(row);
            pane.cursor_row = row;
            pane.cursor_col = col;
        }
    }

    fn next_search_match(&mut self) {
        if self.search_matches.is_empty() {
            self.report_if_no_matches();
            return;
        }
        self.search_match_idx = (self.search_match_idx + 1) % self.search_matches.len();
        self.jump_to_current_match();
    }

    fn prev_search_match(&mut self) {
        if self.search_matches.is_empty() {
            self.report_if_no_matches();
            return;
        }
        self.search_match_idx = self
            .search_match_idx
            .checked_sub(1)
            .unwrap_or(self.search_matches.len() - 1);
        self.jump_to_current_match();
    }

    // ── Visual mode ───────────────────────────────────────────────────────────

    /// Start (or switch) a visual selection anchored at the cursor. Line-wise
    /// selections anchor at column 0.
    fn enter_visual(&mut self, line_wise: bool) {
        let pane = self.pane();
        let col = if line_wise { 0 } else { pane.cursor_col };
        self.visual_anchor = Some((pane.cursor_row, col));
        self.mode = Mode::Visual { line_wise };
    }

    fn exit_visual(&mut self) {
        self.mode = Mode::Normal;
        self.visual_anchor = None;
    }

    fn handle_visual(&mut self, key: KeyEvent, line_wise: bool) {
        // Movement extends the selection.
        if self.apply_motion(key) {
            return;
        }
        let cursor = (self.pane().cursor_row, self.pane().cursor_col);
        let anchor = self.visual_anchor.unwrap_or(cursor);
        match key.code {
            KeyCode::Esc => self.exit_visual(),
            // gg in visual (single keypress is fine here — no ambiguity needed)
            KeyCode::Char('g') => self.pane_mut().move_file_start(),
            // Operators on selection
            KeyCode::Char('d' | 'x') => {
                self.register = if line_wise {
                    self.pane_mut().delete_lines(anchor.0, cursor.0)
                } else {
                    self.pane_mut().delete_char_selection(anchor, cursor)
                };
                self.exit_visual();
            }
            KeyCode::Char('y') => {
                // Char-wise yank is simplified to full lines.
                self.register = self.pane().yank_lines(anchor.0, cursor.0);
                self.message = Some(format!("{} lines yanked", self.register.len()));
                self.exit_visual();
            }
            // Toggle between V and v
            KeyCode::Char('v') if line_wise => self.enter_visual(false),
            KeyCode::Char('V') if !line_wise => self.enter_visual(true),
            _ => {}
        }
    }

    // ── Window management ─────────────────────────────────────────────────────

    fn split(&mut self, dir: SplitDir) {
        let new_pane = match &self.panes[self.active_pane].file_path {
            Some(p) => Pane::from_file(p).unwrap_or_else(|_| Pane::empty()),
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
        let edges = |r: &Rect| {
            (
                i32::from(r.x),
                i32::from(r.x) + i32::from(r.width),
                i32::from(r.y),
                i32::from(r.y) + i32::from(r.height),
            )
        };
        let (cur_l, cur_r, cur_t, cur_b) = edges(&self.pane_rects[self.active_pane]);
        let (cur_cx, cur_cy) = ((cur_l + cur_r) / 2, (cur_t + cur_b) / 2);

        let best = self
            .pane_rects
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != self.active_pane)
            .filter_map(|(i, rect)| {
                let (l, r, t, b) = edges(rect);
                let in_direction = match dir {
                    PaneDir::Left => r <= cur_l,
                    PaneDir::Right => l >= cur_r,
                    PaneDir::Up => b <= cur_t,
                    PaneDir::Down => t >= cur_b,
                };
                // Primary-axis gap dominates; cross-axis offset breaks ties so
                // the best-aligned neighbor wins over a merely closer one.
                let (dx, dy) = (((l + r) / 2 - cur_cx).abs(), ((t + b) / 2 - cur_cy).abs());
                let dist = match dir {
                    PaneDir::Left | PaneDir::Right => dx + dy * 4,
                    PaneDir::Up | PaneDir::Down => dy + dx * 4,
                };
                in_direction.then_some((i, dist))
            })
            .min_by_key(|&(_, dist)| dist);
        if let Some((idx, _)) = best {
            self.active_pane = idx;
        }
    }

    // ── File I/O ──────────────────────────────────────────────────────────────

    /// Resolve a link target against the active pane's file directory, so
    /// relative links (e.g. `[[notes/foo.org]]`) work regardless of the
    /// process's current working directory.
    fn resolve_link_path(&self, raw: &str) -> PathBuf {
        let p = Path::new(raw);
        if p.is_absolute() {
            return p.to_path_buf();
        }
        match self.pane().file_path.as_ref().and_then(|f| f.parent()) {
            Some(dir) if !dir.as_os_str().is_empty() => dir.join(p),
            _ => p.to_path_buf(),
        }
    }

    /// Replace the active pane's buffer with `pane`, recording it as a buffer.
    fn install_pane(&mut self, pane: Pane) {
        if let Some(p) = pane.file_path.clone() {
            self.record_buffer(p);
        }
        self.panes[self.active_pane] = pane;
        self.visual_anchor = None;
        self.key_seq.clear();
        self.mode = Mode::Normal;
    }

    fn open_file(&mut self, path: &Path, force: bool) {
        if !force && self.pane().modified {
            self.message = Some("Unsaved changes — :w first, or :e! to discard".into());
            return;
        }
        match Pane::from_file(path) {
            Ok(pane) => self.install_pane(pane),
            Err(e) => {
                self.message = Some(format!("Cannot open \"{}\": {e}", path.display()));
            }
        }
    }

    // ── Journal ───────────────────────────────────────────────────────────────

    /// Journal date of the active pane's file, if it lives in the journal dir.
    fn current_journal_date(&self) -> Option<clignote_core::journal::Date> {
        let dir = self.config.journal_dir.as_ref()?;
        journal::date_of_path(
            self.pane().file_path.as_ref()?,
            dir,
            &self.config.journal_file_format,
        )
    }

    /// Open (or create) today's journal file. With `new_entry`, also start a
    /// timed entry at its end and enter insert mode; otherwise just show the day.
    fn journal_open_today(&mut self, new_entry: bool) {
        use chrono::{Datelike, Timelike};
        use clignote_core::journal::{timed_headline, Date};

        let Some(dir) = self.config.journal_dir.clone() else {
            self.message = Some("Journal: set [journal] dir in config (no $HOME)".into());
            return;
        };
        let now = chrono::Local::now();
        let Some(today) = Date::new(now.year(), now.month(), now.day()) else {
            self.message = Some("Journal: cannot determine today's date".into());
            return;
        };
        let path = dir.join(today.file_name(&self.config.journal_file_format));
        let in_place = self.pane().file_path.as_deref() == Some(path.as_path());
        if !in_place && self.pane().modified {
            // Switching away would drop the edits, so write them out first.
            if let Err(e) = self.panes[self.active_pane].save() {
                self.message = Some(format!("Unsaved changes could not be saved: {e}"));
                return;
            }
        }
        let mut pane = if in_place {
            // Already showing today's file: append to the live buffer.
            std::mem::replace(&mut self.panes[self.active_pane], Pane::empty())
        } else if path.exists() {
            match Pane::from_file(&path) {
                Ok(p) => p,
                Err(e) => {
                    self.message = Some(format!("Cannot open \"{}\": {e}", path.display()));
                    return;
                }
            }
        } else {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                self.message = Some(format!("Cannot create \"{}\": {e}", dir.display()));
                return;
            }
            Pane::new_file(path, vec![today.headline()])
        };
        if new_entry {
            if self.config.journal_blank_above
                && pane.lines.last().is_some_and(|l| !l.trim().is_empty())
            {
                pane.append_line(String::new());
            }
            let headline =
                timed_headline(self.config.journal_entry_level, now.hour(), now.minute());
            // Text goes on its own line under the heading.
            pane.append_line(headline.trim_end().to_string());
            if self.config.journal_blank_below {
                pane.append_line(String::new());
            }
            pane.append_line(String::new());
        }
        self.install_pane(pane);
        if new_entry {
            self.mode = Mode::Insert;
        }
    }

    /// Open the next/previous existing journal day relative to the current file.
    fn journal_step(&mut self, forward: bool) {
        let (Some(dir), Some(current)) =
            (self.config.journal_dir.clone(), self.current_journal_date())
        else {
            self.message = Some("Not in a journal file".into());
            return;
        };
        let dates = journal::list_dates(&dir, &self.config.journal_file_format);
        match clignote_core::journal::neighbor(&dates, current, forward) {
            Some(date) => self.open_file(
                &dir.join(date.file_name(&self.config.journal_file_format)),
                false,
            ),
            None => {
                self.message = Some(
                    if forward {
                        "No next journal entry"
                    } else {
                        "No previous journal entry"
                    }
                    .into(),
                );
            }
        }
    }

    // ── Buffer list ───────────────────────────────────────────────────────────

    /// Move `path` to the front of the MRU buffer list, deduping.
    fn record_buffer(&mut self, path: PathBuf) {
        self.buffers.retain(|p| p != &path);
        self.buffers.insert(0, path);
    }

    /// Index in the MRU list of the active pane's file, if it is listed.
    fn active_buffer_index(&self) -> Option<usize> {
        let active = self.pane().file_path.as_ref()?;
        self.buffers.iter().position(|p| p == active)
    }

    fn enter_buffer_list(&mut self) {
        self.buf_list_selected = self.active_buffer_index().unwrap_or(0);
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
                select_down(&mut self.buf_list_selected, self.buffers.len(), 1);
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
                self.open_file(&path, false);
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
        let len = self.buffers.len() as isize;
        let next_idx = self
            .active_buffer_index()
            .map_or(0, |idx| (idx as isize + dir).rem_euclid(len) as usize);
        let path = self.buffers[next_idx].clone();
        self.open_file(&path, false);
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
            Some(next) => self.open_file(&next, false),
            None => self.panes[self.active_pane] = Pane::empty(),
        }
        self.message = Some(format!("Killed buffer {}", path.display()));
    }

    // ── Directory browser ─────────────────────────────────────────────────────

    /// List `dir` and switch to `Mode::Browse`. Shows subdirectories and
    /// `.org` files only, directories first, alphabetically within each group.
    fn enter_browse(&mut self, dir: &Path) {
        let read_dir = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) => {
                self.message = Some(format!("Cannot open \"{}\": {e}", dir.display()));
                return;
            }
        };
        let mut entries: Vec<BrowseEntry> = read_dir
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                if name.starts_with('.') {
                    return None;
                }
                let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
                (is_dir || name.ends_with(".org")).then_some(BrowseEntry { name, is_dir })
            })
            .collect();
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
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

    fn browse_parent(&mut self) {
        if let Some(parent) = self.browse_dir.parent().map(Path::to_path_buf) {
            self.enter_browse(&parent);
        }
    }

    fn handle_browse(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let len = self.browse_entries.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => select_down(&mut self.browse_selected, len, 1),
            KeyCode::Char('k') | KeyCode::Up => {
                self.browse_selected = self.browse_selected.saturating_sub(1);
            }
            KeyCode::Char('d') if ctrl => select_down(&mut self.browse_selected, len, 20),
            KeyCode::Char('u') if ctrl => {
                self.browse_selected = self.browse_selected.saturating_sub(20);
            }
            KeyCode::Char('-') | KeyCode::Backspace => self.browse_parent(),
            KeyCode::Enter => {
                let Some(entry) = self.browse_entries.get(self.browse_selected) else {
                    return;
                };
                if entry.name == ".." {
                    self.browse_parent();
                    return;
                }
                let target = self.browse_dir.join(&entry.name);
                if entry.is_dir {
                    self.enter_browse(&target);
                } else {
                    self.open_file(&target, false);
                }
            }
            _ => {}
        }
    }

    fn save_active(&mut self) {
        let result = self.panes[self.active_pane].save();
        self.message = Some(result.unwrap_or_else(|e| e));
    }

    /// Save, then commit the file with a `YYYY-MM-DD HH:MM` message and push
    /// to the current branch's upstream (or `origin`). The git work runs on a
    /// worker thread so the UI can show a spinner; see `poll_commit_job`.
    fn save_commit_push(&mut self) {
        if self.commit_job.is_some() {
            return;
        }
        let saved = match self.panes[self.active_pane].save() {
            Ok(m) => m,
            Err(e) => {
                self.message = Some(e);
                return;
            }
        };
        let Some(path) = self.pane().file_path.clone() else {
            return;
        };
        let msg = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let (job_path, job_msg) = (path.clone(), msg.clone());
        std::thread::spawn(move || {
            let _ = tx.send(git_commit_push(&job_path, &job_msg));
        });
        self.commit_job = Some(CommitJob {
            rx,
            started: std::time::Instant::now(),
            saved,
            msg,
            path,
        });
    }

    /// Collect a finished commit-and-push. Returns `true` when the job just
    /// completed (the screen needs a redraw either way while one is running).
    pub fn poll_commit_job(&mut self) -> bool {
        let Some(job) = &self.commit_job else {
            return false;
        };
        let result = match job.rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("git job died".to_string()),
        };
        let Some(job) = self.commit_job.take() else {
            return false;
        };
        // The commit moved HEAD: re-baseline the gutter so committed lines
        // stop showing as added/modified.
        for pane in &mut self.panes {
            if pane.file_path.as_deref() == Some(job.path.as_path()) {
                pane.refresh_head_lines();
            }
        }
        self.message = Some(match result {
            Ok(out) => format!("{}; {out} ({})", job.saved, job.msg),
            Err(e) => format!("{}; git: {e}", job.saved),
        });
        true
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

/// A commit-and-push running on a worker thread.
pub struct CommitJob {
    rx: std::sync::mpsc::Receiver<Result<String, String>>,
    started: std::time::Instant,
    saved: String,
    msg: String,
    path: PathBuf,
}

impl CommitJob {
    /// A spinner frame plus elapsed seconds, e.g. `⠹ committing and pushing… 3s`.
    pub fn spinner_line(&self) -> String {
        const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let elapsed = self.started.elapsed();
        let frame = (elapsed.as_millis() / 100) as usize % FRAMES.len();
        format!(
            "{} committing and pushing… {}s",
            FRAMES[frame],
            elapsed.as_secs()
        )
    }
}

/// Run `git` in `dir`, returning trimmed stdout or a stderr-based error.
fn git(dir: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = if err.trim().is_empty() {
            String::from_utf8_lossy(&out.stdout)
        } else {
            err
        };
        Err(err.trim().lines().next().unwrap_or("failed").to_string())
    }
}

/// Stage and commit `path` (skipped when unchanged), then push.
fn git_commit_push(path: &std::path::Path, message: &str) -> Result<String, String> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let file = path
        .file_name()
        .and_then(|f| f.to_str())
        .ok_or("invalid file name")?;
    git(dir, &["add", "--", file])?;
    let committed = git(dir, &["diff", "--cached", "--quiet", "--", file]).is_err();
    if committed {
        git(dir, &["commit", "-m", message, "--", file])?;
    }
    git(dir, &["push"]).or_else(|_| git(dir, &["push", "origin", "HEAD"]))?;
    Ok(if committed {
        "committed and pushed".into()
    } else {
        "nothing to commit, pushed".into()
    })
}
