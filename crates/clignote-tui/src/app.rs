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
            let completed = if search_dir == StdPathBuf::from(".") && !partial.starts_with("./") {
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
    /// `line_wise = true` → V (line visual), false → v (char visual)
    Visual {
        line_wise: bool,
    },
}

impl Mode {
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Command => "COMMAND",
            Mode::Visual { line_wise: true } => "V-LINE",
            Mode::Visual { line_wise: false } => "VISUAL",
        }
    }
}

// ── Split layout ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitLayout {
    Single,
    /// pane[0] left │ pane[1] right
    Horizontal,
    /// pane[0] top / pane[1] bottom
    Vertical,
}

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    pub mode: Mode,
    pub panes: Vec<Pane>,
    pub active_pane: usize,
    pub layout: SplitLayout,

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
}

impl App {
    pub fn new(file_path: Option<&str>) -> anyhow::Result<Self> {
        let pane = match file_path {
            Some(p) => Pane::from_file(p)?,
            None => Pane::empty(),
        };
        Ok(Self {
            mode: Mode::Normal,
            panes: vec![pane],
            active_pane: 0,
            layout: SplitLayout::Single,
            register: Vec::new(),
            visual_anchor: None,
            key_seq: Vec::new(),
            command_buf: String::new(),
            message: None,
            should_quit: false,
            pane_rects: Vec::new(),
            completions: None,
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
            Mode::Visual { line_wise } => self.handle_visual(key, *line_wise),
        }
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) {
        if let MouseEventKind::Down(MouseButton::Left) = event.kind {
            self.click_at(event.column as usize, event.row as usize);
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
                let buf_row = (pane.viewport_top + row - rect.y as usize)
                    .min(pane.lines.len().saturating_sub(1));
                let buf_col =
                    (col - rect.x as usize).min(pane.lines[buf_row].len().saturating_sub(1));
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
                return; // keep accumulating
            }
            MatchResult::Action(action) => {
                self.key_seq.clear();
                self.dispatch_action(action);
                return;
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

            // ── Misc ──────────────────────────────────────────────────────────
            KeyCode::Esc => {
                self.key_seq.clear();
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
            Action::YankLine => {
                let line = self.pane().yank_line();
                self.register = vec![line];
                self.message = Some("1 line yanked".into());
            }
            Action::SplitHorizontal => self.split(SplitLayout::Horizontal),
            Action::SplitVertical => self.split(SplitLayout::Vertical),
            Action::ClosePane => self.close_active_pane(),
            Action::NextPane => self.cycle_pane(),
            Action::FocusPane(dir) => self.focus_pane(dir),
            Action::SaveFile => self.save_active(),
            Action::QuitAll => {
                if self.panes.iter().any(|p| p.modified) {
                    self.message = Some("Unsaved changes — use :q! or :wq".into());
                } else {
                    self.should_quit = true;
                }
            }
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
            "sp" | "split" => self.split(SplitLayout::Horizontal),
            "vs" | "vsplit" => self.split(SplitLayout::Vertical),
            "e" | "edit" => {
                if let Some(path) = parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                    self.open_file(path, false);
                } else {
                    self.message = Some("Usage: :e <filename>".into());
                }
            }
            "e!" | "edit!" => {
                if let Some(path) = parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                    self.open_file(path, true);
                } else {
                    self.message = Some("Usage: :e! <filename>".into());
                }
            }
            other => {
                self.message = Some(format!("Unknown command: {}", other));
            }
        }
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

    fn split(&mut self, layout: SplitLayout) {
        if self.panes.len() >= 2 {
            self.message = Some("At most 2 panes supported".into());
            return;
        }
        let new_pane = match &self.panes[self.active_pane].file_path {
            Some(p) => Pane::from_file(p.to_str().unwrap_or("")).unwrap_or_else(|_| Pane::empty()),
            None => Pane::empty(),
        };
        self.panes.push(new_pane);
        self.layout = layout;
        self.active_pane = 1; // focus the new pane
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
        self.panes.remove(self.active_pane);
        self.layout = SplitLayout::Single;
        self.active_pane = 0;
    }

    fn cycle_pane(&mut self) {
        if self.panes.len() > 1 {
            self.active_pane = (self.active_pane + 1) % self.panes.len();
        }
    }

    fn focus_pane(&mut self, dir: PaneDir) {
        if self.panes.len() < 2 {
            return;
        }
        match (&self.layout, dir) {
            // Horizontal split: panes stacked, navigate up/down
            (SplitLayout::Horizontal, PaneDir::Down) => self.active_pane = 1,
            (SplitLayout::Horizontal, PaneDir::Up) => self.active_pane = 0,
            // Vertical split: panes side by side, navigate left/right
            (SplitLayout::Vertical, PaneDir::Right) => self.active_pane = 1,
            (SplitLayout::Vertical, PaneDir::Left) => self.active_pane = 0,
            _ => {}
        }
    }

    // ── File I/O ──────────────────────────────────────────────────────────────

    fn open_file(&mut self, path: &str, force: bool) {
        if !force && self.pane().modified {
            self.message = Some("Unsaved changes — :w first, or :e! to discard".into());
            return;
        }
        match Pane::from_file(path) {
            Ok(pane) => {
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
