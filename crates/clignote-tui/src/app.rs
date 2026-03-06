use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Command,
}

impl Mode {
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Command => "COMMAND",
        }
    }
}

pub struct App {
    pub mode: Mode,
    /// Raw lines of the open file.
    pub lines: Vec<String>,
    /// 0-based cursor row.
    pub cursor_row: usize,
    /// 0-based cursor column (byte index, clamped to line length).
    pub cursor_col: usize,
    /// First visible line (scroll offset).
    pub viewport_top: usize,
    pub file_path: Option<PathBuf>,
    pub modified: bool,
    /// Content of the command-mode input (after `:`).
    pub command_buf: String,
    /// One-line status message displayed in the footer.
    pub message: Option<String>,
    pub should_quit: bool,
    /// Pending normal-mode operator (e.g. 'd' waiting for second 'd').
    pending_op: Option<char>,
    /// Default register (for yank/delete).
    pub register: Vec<String>,
}

impl App {
    pub fn new(file_path: Option<&str>) -> anyhow::Result<Self> {
        let (lines, path) = match file_path {
            Some(p) => {
                let content = std::fs::read_to_string(p)?;
                let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
                // Ensure at least one line so the cursor is always valid
                let lines = if lines.is_empty() { vec![String::new()] } else { lines };
                (lines, Some(PathBuf::from(p)))
            }
            None => (vec![String::new()], None),
        };

        Ok(Self {
            mode: Mode::Normal,
            lines,
            cursor_row: 0,
            cursor_col: 0,
            viewport_top: 0,
            file_path: path,
            modified: false,
            command_buf: String::new(),
            message: None,
            should_quit: false,
            pending_op: None,
            register: Vec::new(),
        })
    }

    // ── Input dispatch ────────────────────────────────────────────────────────

    pub fn handle_key(&mut self, key: KeyEvent) {
        // Clear transient message on any keypress
        self.message = None;

        match self.mode {
            Mode::Normal => self.handle_normal(key),
            Mode::Insert => self.handle_insert(key),
            Mode::Command => self.handle_command(key),
        }
    }

    // ── Normal mode ───────────────────────────────────────────────────────────

    fn handle_normal(&mut self, key: KeyEvent) {
        match key.code {
            // ── Movement ─────────────────────────────────────────────────────
            KeyCode::Char('h') | KeyCode::Left => self.move_left(),
            KeyCode::Char('j') | KeyCode::Down => self.move_down(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_up(1),
            KeyCode::Char('l') | KeyCode::Right => self.move_right(),

            // Word motions (basic: jump to next/prev whitespace boundary)
            KeyCode::Char('w') => self.move_word_forward(),
            KeyCode::Char('b') => self.move_word_backward(),

            // Line boundaries
            KeyCode::Char('0') => self.cursor_col = 0,
            KeyCode::Char('$') | KeyCode::End => {
                self.cursor_col = self.current_line_len().saturating_sub(1);
            }

            // File boundaries
            KeyCode::Char('G') => {
                self.cursor_row = self.lines.len().saturating_sub(1);
                self.clamp_col();
                self.pending_op = None;
            }
            KeyCode::Char('g') => {
                if self.pending_op == Some('g') {
                    self.cursor_row = 0;
                    self.cursor_col = 0;
                    self.pending_op = None;
                } else {
                    self.pending_op = Some('g');
                    return; // don't clear pending_op below
                }
            }

            // Ctrl+d / Ctrl+u half-page scroll
            KeyCode::Char('d') if key.modifiers == KeyModifiers::CONTROL => {
                let half = self.half_page();
                self.move_down(half);
            }
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                let half = self.half_page();
                self.move_up(half);
            }

            // ── Operators: dd, yy ────────────────────────────────────────────
            KeyCode::Char('d') => {
                if self.pending_op == Some('d') {
                    self.delete_line();
                    self.pending_op = None;
                } else {
                    self.pending_op = Some('d');
                    return;
                }
            }
            KeyCode::Char('y') => {
                if self.pending_op == Some('y') {
                    self.yank_line();
                    self.pending_op = None;
                } else {
                    self.pending_op = Some('y');
                    return;
                }
            }

            // Paste
            KeyCode::Char('p') => self.paste_after(),
            KeyCode::Char('P') => self.paste_before(),

            // ── Mode switches ─────────────────────────────────────────────────
            KeyCode::Char('i') => self.enter_insert(false),
            KeyCode::Char('a') => {
                self.cursor_col = self.cursor_col.saturating_add(1).min(self.current_line_len());
                self.enter_insert(false);
            }
            KeyCode::Char('o') => {
                let row = self.cursor_row + 1;
                self.lines.insert(row, String::new());
                self.cursor_row = row;
                self.cursor_col = 0;
                self.enter_insert(false);
            }
            KeyCode::Char('O') => {
                self.lines.insert(self.cursor_row, String::new());
                self.cursor_col = 0;
                self.enter_insert(false);
            }
            KeyCode::Char(':') => {
                self.command_buf.clear();
                self.mode = Mode::Command;
                return;
            }

            _ => {}
        }

        // Any unhandled key clears a pending operator
        self.pending_op = None;
    }

    // ── Insert mode ───────────────────────────────────────────────────────────

    fn handle_insert(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                // Move cursor back one column on leaving insert (vim behaviour)
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                }
                self.mode = Mode::Normal;
            }
            KeyCode::Char(c) => {
                let col = self.cursor_col;
                self.lines[self.cursor_row].insert(col, c);
                self.cursor_col += 1;
                self.modified = true;
            }
            KeyCode::Backspace => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                    self.lines[self.cursor_row].remove(self.cursor_col);
                    self.modified = true;
                } else if self.cursor_row > 0 {
                    // Join with previous line
                    let current = self.lines.remove(self.cursor_row);
                    self.cursor_row -= 1;
                    self.cursor_col = self.lines[self.cursor_row].len();
                    self.lines[self.cursor_row].push_str(&current);
                    self.modified = true;
                }
            }
            KeyCode::Enter => {
                let col = self.cursor_col;
                let rest = self.lines[self.cursor_row].split_off(col);
                self.cursor_row += 1;
                self.lines.insert(self.cursor_row, rest);
                self.cursor_col = 0;
                self.modified = true;
            }
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Up => self.move_up(1),
            KeyCode::Down => self.move_down(1),
            _ => {}
        }
    }

    // ── Command mode ──────────────────────────────────────────────────────────

    fn handle_command(&mut self, key: KeyEvent) {
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
                    // Empty command buf → leave command mode
                    self.mode = Mode::Normal;
                }
            }
            KeyCode::Char(c) => {
                self.command_buf.push(c);
            }
            _ => {}
        }
    }

    fn execute_command(&mut self, cmd: &str) {
        match cmd.trim() {
            "q" => {
                if self.modified {
                    self.message = Some("Unsaved changes — use :q! to force quit or :wq to save".into());
                } else {
                    self.should_quit = true;
                }
            }
            "q!" => self.should_quit = true,
            "w" => self.save(),
            "wq" | "x" => {
                self.save();
                self.should_quit = true;
            }
            other => {
                self.message = Some(format!("Unknown command: {}", other));
            }
        }
    }

    // ── Movement helpers ──────────────────────────────────────────────────────

    fn move_left(&mut self) {
        self.cursor_col = self.cursor_col.saturating_sub(1);
    }

    fn move_right(&mut self) {
        let max = self.current_line_len().saturating_sub(1);
        self.cursor_col = (self.cursor_col + 1).min(max);
    }

    fn move_up(&mut self, n: usize) {
        self.cursor_row = self.cursor_row.saturating_sub(n);
        self.clamp_col();
    }

    fn move_down(&mut self, n: usize) {
        self.cursor_row = (self.cursor_row + n).min(self.lines.len().saturating_sub(1));
        self.clamp_col();
    }

    fn move_word_forward(&mut self) {
        let line = &self.lines[self.cursor_row];
        let mut col = self.cursor_col;
        let bytes = line.as_bytes();
        // Skip current word chars
        while col < bytes.len() && !bytes[col].is_ascii_whitespace() {
            col += 1;
        }
        // Skip whitespace
        while col < bytes.len() && bytes[col].is_ascii_whitespace() {
            col += 1;
        }
        self.cursor_col = col.min(line.len().saturating_sub(1));
    }

    fn move_word_backward(&mut self) {
        let line = &self.lines[self.cursor_row];
        let bytes = line.as_bytes();
        let mut col = self.cursor_col;
        if col == 0 {
            return;
        }
        col -= 1;
        // Skip whitespace going left
        while col > 0 && bytes[col].is_ascii_whitespace() {
            col -= 1;
        }
        // Skip word chars going left
        while col > 0 && !bytes[col - 1].is_ascii_whitespace() {
            col -= 1;
        }
        self.cursor_col = col;
    }

    fn clamp_col(&mut self) {
        let max = self.current_line_len().saturating_sub(1);
        self.cursor_col = self.cursor_col.min(max);
    }

    fn current_line_len(&self) -> usize {
        self.lines.get(self.cursor_row).map(|l| l.len()).unwrap_or(0)
    }

    fn half_page(&self) -> usize {
        20 // approximate; ui.rs adjusts the viewport using terminal height
    }

    // ── Edit helpers ──────────────────────────────────────────────────────────

    fn enter_insert(&mut self, _prepend: bool) {
        self.mode = Mode::Insert;
    }

    fn delete_line(&mut self) {
        if self.lines.len() == 1 {
            self.register = vec![self.lines[0].clone()];
            self.lines[0].clear();
        } else {
            let removed = self.lines.remove(self.cursor_row);
            self.register = vec![removed];
            if self.cursor_row >= self.lines.len() {
                self.cursor_row = self.lines.len().saturating_sub(1);
            }
        }
        self.clamp_col();
        self.modified = true;
    }

    fn yank_line(&mut self) {
        if let Some(line) = self.lines.get(self.cursor_row) {
            self.register = vec![line.clone()];
        }
        self.message = Some("1 line yanked".into());
    }

    fn paste_after(&mut self) {
        if self.register.is_empty() {
            return;
        }
        for (i, line) in self.register.iter().enumerate() {
            self.lines.insert(self.cursor_row + 1 + i, line.clone());
        }
        self.cursor_row += 1;
        self.modified = true;
    }

    fn paste_before(&mut self) {
        if self.register.is_empty() {
            return;
        }
        for (i, line) in self.register.iter().enumerate() {
            self.lines.insert(self.cursor_row + i, line.clone());
        }
        self.modified = true;
    }

    // ── File I/O ──────────────────────────────────────────────────────────────

    fn save(&mut self) {
        match &self.file_path {
            None => {
                self.message = Some("No file name — use :w <filename>".into());
            }
            Some(path) => {
                let content = self.lines.join("\n") + "\n";
                match std::fs::write(path, content) {
                    Ok(_) => {
                        self.modified = false;
                        let name = path.display().to_string();
                        self.message = Some(format!("\"{}\" written", name));
                    }
                    Err(e) => {
                        self.message = Some(format!("Error writing file: {}", e));
                    }
                }
            }
        }
    }

    // ── Viewport ──────────────────────────────────────────────────────────────

    /// Adjust `viewport_top` so that `cursor_row` is visible within `height` lines.
    pub fn scroll_to_cursor(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.cursor_row < self.viewport_top {
            self.viewport_top = self.cursor_row;
        } else if self.cursor_row >= self.viewport_top + height {
            self.viewport_top = self.cursor_row - height + 1;
        }
    }
}
