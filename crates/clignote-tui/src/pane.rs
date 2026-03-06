use std::path::PathBuf;

/// One editor pane: its own buffer, cursor, and viewport.
pub struct Pane {
    pub lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub viewport_top: usize,
    pub file_path: Option<PathBuf>,
    pub modified: bool,
}

impl Pane {
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
        let lines = if lines.is_empty() {
            vec![String::new()]
        } else {
            lines
        };
        Ok(Self {
            lines,
            cursor_row: 0,
            cursor_col: 0,
            viewport_top: 0,
            file_path: Some(PathBuf::from(path)),
            modified: false,
        })
    }

    pub fn empty() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            viewport_top: 0,
            file_path: None,
            modified: false,
        }
    }

    // ── Geometry ──────────────────────────────────────────────────────────────

    pub fn current_line_len(&self) -> usize {
        self.lines
            .get(self.cursor_row)
            .map(|l| l.len())
            .unwrap_or(0)
    }

    pub fn clamp_col(&mut self) {
        let max = self.current_line_len().saturating_sub(1);
        self.cursor_col = self.cursor_col.min(max);
    }

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

    // ── Movement ──────────────────────────────────────────────────────────────

    pub fn move_left(&mut self) {
        self.cursor_col = self.cursor_col.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        let max = self.current_line_len().saturating_sub(1);
        if self.cursor_col < max {
            self.cursor_col += 1;
        }
    }

    pub fn move_up(&mut self, n: usize) {
        self.cursor_row = self.cursor_row.saturating_sub(n);
        self.clamp_col();
    }

    pub fn move_down(&mut self, n: usize) {
        self.cursor_row = (self.cursor_row + n).min(self.lines.len().saturating_sub(1));
        self.clamp_col();
    }

    pub fn move_word_forward(&mut self) {
        let bytes = self.lines[self.cursor_row].as_bytes();
        let mut col = self.cursor_col;
        while col < bytes.len() && !bytes[col].is_ascii_whitespace() {
            col += 1;
        }
        while col < bytes.len() && bytes[col].is_ascii_whitespace() {
            col += 1;
        }
        self.cursor_col = col.min(bytes.len().saturating_sub(1));
    }

    pub fn move_word_backward(&mut self) {
        let bytes = self.lines[self.cursor_row].as_bytes();
        let mut col = self.cursor_col;
        if col == 0 {
            return;
        }
        col -= 1;
        while col > 0 && bytes[col].is_ascii_whitespace() {
            col -= 1;
        }
        while col > 0 && !bytes[col - 1].is_ascii_whitespace() {
            col -= 1;
        }
        self.cursor_col = col;
    }

    pub fn move_line_start(&mut self) {
        self.cursor_col = 0;
    }

    pub fn move_line_end(&mut self) {
        self.cursor_col = self.current_line_len().saturating_sub(1);
    }

    pub fn move_file_start(&mut self) {
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    pub fn move_file_end(&mut self) {
        self.cursor_row = self.lines.len().saturating_sub(1);
        self.clamp_col();
    }

    // ── Editing ───────────────────────────────────────────────────────────────

    /// Delete the current line and return it. Leaves at least one line.
    pub fn delete_line(&mut self) -> String {
        if self.lines.len() == 1 {
            let removed = self.lines[0].clone();
            self.lines[0].clear();
            self.cursor_col = 0;
            self.modified = true;
            return removed;
        }
        let removed = self.lines.remove(self.cursor_row);
        if self.cursor_row >= self.lines.len() {
            self.cursor_row = self.lines.len().saturating_sub(1);
        }
        self.clamp_col();
        self.modified = true;
        removed
    }

    /// Delete a range of lines (inclusive). Returns the removed lines.
    pub fn delete_lines(&mut self, start: usize, end: usize) -> Vec<String> {
        let start = start.min(self.lines.len().saturating_sub(1));
        let end = end.min(self.lines.len().saturating_sub(1));
        let (lo, hi) = (start.min(end), start.max(end));

        let removed: Vec<String> = self.lines.drain(lo..=hi).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = lo.min(self.lines.len().saturating_sub(1));
        self.clamp_col();
        self.modified = true;
        removed
    }

    /// Delete a char-wise selection. `start` and `end` are (row, col) pairs.
    pub fn delete_char_selection(
        &mut self,
        (sr, sc): (usize, usize),
        (er, ec): (usize, usize),
    ) -> Vec<String> {
        let (sr, sc, er, ec) = if (sr, sc) <= (er, ec) {
            (sr, sc, er, ec)
        } else {
            (er, ec, sr, sc)
        };

        if sr == er {
            // Single line
            let line = &mut self.lines[sr];
            let ec_clamped = ec.min(line.len().saturating_sub(1));
            let removed = line[sc..=ec_clamped].to_string();
            line.replace_range(sc..=ec_clamped, "");
            self.cursor_row = sr;
            self.cursor_col = sc.min(self.lines[sr].len().saturating_sub(1));
            self.modified = true;
            return vec![removed];
        }

        // Multi-line: keep prefix of start line and suffix of end line
        let start_prefix = self.lines[sr][..sc].to_string();
        let end_suffix = if ec + 1 < self.lines[er].len() {
            self.lines[er][ec + 1..].to_string()
        } else {
            String::new()
        };

        // Collect removed text
        let mut removed: Vec<String> = Vec::new();
        removed.push(self.lines[sr][sc..].to_string());
        for row in sr + 1..er {
            removed.push(self.lines[row].clone());
        }
        removed.push(self.lines[er][..=ec.min(self.lines[er].len().saturating_sub(1))].to_string());

        // Remove lines sr..=er and replace with joined result
        self.lines.drain(sr..=er);
        self.lines
            .insert(sr, format!("{}{}", start_prefix, end_suffix));
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = sr;
        self.cursor_col = sc.min(self.lines[sr].len().saturating_sub(1));
        self.modified = true;
        removed
    }

    pub fn yank_line(&self) -> String {
        self.lines.get(self.cursor_row).cloned().unwrap_or_default()
    }

    pub fn yank_lines(&self, start: usize, end: usize) -> Vec<String> {
        let lo = start.min(end).min(self.lines.len().saturating_sub(1));
        let hi = start.max(end).min(self.lines.len().saturating_sub(1));
        self.lines[lo..=hi].to_vec()
    }

    pub fn paste_lines_after(&mut self, lines: &[String]) {
        let insert_at = self.cursor_row + 1;
        for (i, l) in lines.iter().enumerate() {
            self.lines.insert(insert_at + i, l.clone());
        }
        self.cursor_row = insert_at;
        self.modified = true;
    }

    pub fn paste_lines_before(&mut self, lines: &[String]) {
        for (i, l) in lines.iter().enumerate() {
            self.lines.insert(self.cursor_row + i, l.clone());
        }
        self.modified = true;
    }

    pub fn insert_char(&mut self, c: char) {
        self.lines[self.cursor_row].insert(self.cursor_col, c);
        self.cursor_col += 1;
        self.modified = true;
    }

    pub fn delete_char_before(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
            self.lines[self.cursor_row].remove(self.cursor_col);
            self.modified = true;
        } else if self.cursor_row > 0 {
            let current = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
            self.lines[self.cursor_row].push_str(&current);
            self.modified = true;
        }
    }

    pub fn insert_newline(&mut self) {
        let rest = self.lines[self.cursor_row].split_off(self.cursor_col);
        self.cursor_row += 1;
        self.lines.insert(self.cursor_row, rest);
        self.cursor_col = 0;
        self.modified = true;
    }

    pub fn open_line_below(&mut self) {
        self.lines.insert(self.cursor_row + 1, String::new());
        self.cursor_row += 1;
        self.cursor_col = 0;
        self.modified = true;
    }

    pub fn open_line_above(&mut self) {
        self.lines.insert(self.cursor_row, String::new());
        self.cursor_col = 0;
        self.modified = true;
    }

    // ── Org-mode helpers ──────────────────────────────────────────────────────

    /// Toggle the checkbox on the current line if it is a list item with one.
    /// Cycles: `[ ]` → `[X]` → `[ ]`  (`[-]` also resets to `[ ]`).
    pub fn toggle_checkbox(&mut self) {
        let line = self.lines[self.cursor_row].clone();
        let indent = line.len() - line.trim_start().len();
        let tail = &line[indent..];

        let after_bullet = if tail.starts_with("- ") || tail.starts_with("+ ") {
            indent + 2
        } else {
            let digits = tail.chars().take_while(|c| c.is_ascii_digit()).count();
            if digits > 0 && tail.get(digits..).map_or(false, |s| s.starts_with(". ")) {
                indent + digits + 2
            } else {
                return;
            }
        };

        let rest = &line[after_bullet..];
        let new_cb = if rest.starts_with("[ ] ") {
            "[X]"
        } else if rest.starts_with("[X] ") || rest.starts_with("[x] ") || rest.starts_with("[-] ") {
            "[ ]"
        } else {
            return;
        };

        self.lines[self.cursor_row].replace_range(after_bullet..after_bullet + 3, new_cb);
        self.modified = true;
    }

    // ── Persistence ───────────────────────────────────────────────────────────

    pub fn save(&mut self) -> Result<String, String> {
        match &self.file_path.clone() {
            None => Err("No file name — use :w <filename>".into()),
            Some(path) => {
                let content = self.lines.join("\n") + "\n";
                std::fs::write(path, &content)
                    .map(|_| {
                        self.modified = false;
                        format!("\"{}\" written", path.display())
                    })
                    .map_err(|e| format!("Error writing: {}", e))
            }
        }
    }
}
