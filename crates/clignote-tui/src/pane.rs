use std::path::{Path, PathBuf};

use crate::git::{self, LineStatus};
use crate::links;

/// Shift `offset` the minimum amount needed so `pos` lies in
/// `offset..offset + size`. No-op when `size` is zero.
pub(crate) fn scroll_into_view(offset: &mut usize, pos: usize, size: usize) {
    if size == 0 {
        return;
    }
    if pos < *offset {
        *offset = pos;
    } else if pos >= *offset + size {
        *offset = pos + 1 - size;
    }
}

/// One editor pane: its own buffer, cursor, and viewport.
pub struct Pane {
    pub lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    /// First visible buffer row (vertical scroll).
    pub viewport_top: usize,
    /// First visible buffer column (horizontal scroll).
    pub viewport_left: usize,
    pub file_path: Option<PathBuf>,
    pub modified: bool,
    /// Per-line diff status vs the committed HEAD version.
    pub git_diff: Vec<LineStatus>,
    /// HEAD content fetched on open/save; used for real-time in-memory diff.
    head_lines: Option<Vec<String>>,
    undo_stack: Vec<(Vec<String>, usize, usize)>,
}

impl Pane {
    pub fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)?;
        let mut lines: Vec<String> = content.lines().map(str::to_owned).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        let head_lines = git::get_head_lines(path);
        let git_diff = git::diff_with_head(head_lines.as_deref(), &lines);
        Ok(Self {
            lines,
            file_path: Some(path.to_path_buf()),
            git_diff,
            head_lines,
            ..Self::empty()
        })
    }

    pub fn empty() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            viewport_top: 0,
            viewport_left: 0,
            file_path: None,
            modified: false,
            git_diff: Vec::new(),
            head_lines: None,
            undo_stack: Vec::new(),
        }
    }

    /// Re-diff the current buffer against HEAD. Called from the render loop so
    /// the gutter updates in real-time as the user types.
    pub fn recompute_git_diff(&mut self) {
        self.git_diff = git::diff_with_head(self.head_lines.as_deref(), &self.lines);
    }

    /// Re-fetch HEAD content from git (called after a successful save so that
    /// a subsequent commit will reset the baseline correctly).
    fn refresh_head_lines(&mut self) {
        if let Some(path) = &self.file_path {
            self.head_lines = git::get_head_lines(path);
        }
    }

    // ── Geometry ──────────────────────────────────────────────────────────────

    pub fn current_line_len(&self) -> usize {
        self.lines.get(self.cursor_row).map_or(0, String::len)
    }

    pub fn clamp_col(&mut self) {
        let max = self.current_line_len().saturating_sub(1);
        self.cursor_col = self.cursor_col.min(max);
    }

    /// Keep the cursor inside the visible viewport (vertical and horizontal).
    pub fn scroll_to_cursor(&mut self, height: usize, width: usize) {
        scroll_into_view(&mut self.viewport_top, self.cursor_row, height);
        scroll_into_view(&mut self.viewport_left, self.cursor_col, width);
    }

    pub fn scroll_down(&mut self, n: usize) {
        let max_top = self.lines.len().saturating_sub(1);
        self.viewport_top = (self.viewport_top + n).min(max_top);
        self.cursor_row = self.cursor_row.max(self.viewport_top);
        if self.cursor_row >= self.lines.len() {
            self.cursor_row = self.lines.len().saturating_sub(1);
        }
        self.clamp_col();
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.viewport_top = self.viewport_top.saturating_sub(n);
        self.clamp_col();
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

    // ── Undo ──────────────────────────────────────────────────────────────────

    fn snapshot(&mut self) {
        self.undo_stack
            .push((self.lines.clone(), self.cursor_row, self.cursor_col));
    }

    pub fn undo(&mut self) -> bool {
        if let Some((lines, row, col)) = self.undo_stack.pop() {
            self.lines = lines;
            self.cursor_row = row;
            self.cursor_col = col;
            self.modified = true;
            true
        } else {
            false
        }
    }

    // ── Editing ───────────────────────────────────────────────────────────────

    /// Delete the current line and return it. Leaves at least one line.
    pub fn delete_line(&mut self) -> String {
        self.snapshot();
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
        self.snapshot();
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
        self.snapshot();
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
        let end_line = &self.lines[er];
        let end_suffix = end_line.get(ec + 1..).unwrap_or_default();
        let joined = format!("{}{}", &self.lines[sr][..sc], end_suffix);

        // Collect removed text
        let mut removed: Vec<String> = Vec::with_capacity(er - sr + 1);
        removed.push(self.lines[sr][sc..].to_string());
        removed.extend(self.lines[sr + 1..er].iter().cloned());
        removed.push(end_line[..=ec.min(end_line.len().saturating_sub(1))].to_string());

        // Replace lines sr..=er with the joined result
        self.lines.splice(sr..=er, [joined]);
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
        self.snapshot();
        let insert_at = self.cursor_row + 1;
        self.lines
            .splice(insert_at..insert_at, lines.iter().cloned());
        self.cursor_row = insert_at;
        self.modified = true;
    }

    pub fn paste_lines_before(&mut self, lines: &[String]) {
        self.snapshot();
        let at = self.cursor_row;
        self.lines.splice(at..at, lines.iter().cloned());
        self.modified = true;
    }

    pub fn insert_char(&mut self, c: char) {
        self.snapshot();
        self.lines[self.cursor_row].insert(self.cursor_col, c);
        self.cursor_col += 1;
        self.modified = true;
    }

    /// Delete the character under the cursor (vim `x`).
    pub fn delete_char_at_cursor(&mut self) {
        if self.lines[self.cursor_row].is_empty() {
            return;
        }
        self.snapshot();
        let line = &mut self.lines[self.cursor_row];
        let col = self.cursor_col.min(line.len() - 1);
        line.remove(col);
        self.cursor_col = col.min(line.len().saturating_sub(1));
        self.modified = true;
    }

    /// Delete from cursor to end of current word (vim `dw`).
    pub fn delete_word(&mut self) {
        let bytes = self.lines[self.cursor_row].as_bytes();
        let col = self.cursor_col;
        if col >= bytes.len() {
            return;
        }
        // Mirror move_word_forward: skip non-whitespace then whitespace
        let mut end = col;
        while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
            end += 1;
        }
        while end < bytes.len() && bytes[end].is_ascii_whitespace() {
            end += 1;
        }
        self.snapshot();
        let line = &mut self.lines[self.cursor_row];
        line.replace_range(col..end, "");
        self.cursor_col = col.min(line.len().saturating_sub(1));
        self.modified = true;
    }

    pub fn delete_char_before(&mut self) {
        if self.cursor_col == 0 && self.cursor_row == 0 {
            return;
        }
        self.snapshot();
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
            self.lines[self.cursor_row].remove(self.cursor_col);
            self.modified = true;
        } else {
            let current = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
            self.lines[self.cursor_row].push_str(&current);
            self.modified = true;
        }
    }

    pub fn insert_newline(&mut self) {
        self.snapshot();
        let rest = self.lines[self.cursor_row].split_off(self.cursor_col);
        self.cursor_row += 1;
        self.lines.insert(self.cursor_row, rest);
        self.cursor_col = 0;
        self.modified = true;
    }

    pub fn open_line_below(&mut self) {
        self.snapshot();
        self.lines.insert(self.cursor_row + 1, String::new());
        self.cursor_row += 1;
        self.cursor_col = 0;
        self.modified = true;
    }

    pub fn open_line_above(&mut self) {
        self.snapshot();
        self.lines.insert(self.cursor_row, String::new());
        self.cursor_col = 0;
        self.modified = true;
    }

    // ── Org-mode helpers ──────────────────────────────────────────────────────

    /// Return the URL of the `[[url]]` or `[[url][desc]]` link under the cursor,
    /// or `None` if the cursor is not inside a link.
    pub fn link_at_cursor(&self) -> Option<String> {
        let line = self.lines.get(self.cursor_row)?;
        let col = self.cursor_col;
        links::find_links(line.as_bytes(), b'[', b']')
            .into_iter()
            .find(|l| (l.start..=l.end).contains(&col))
            .map(|l| line[l.url].to_string())
    }

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
            if digits > 0 && tail.get(digits..).is_some_and(|s| s.starts_with(". ")) {
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

        self.snapshot();
        self.lines[self.cursor_row].replace_range(after_bullet..after_bullet + 3, new_cb);
        self.modified = true;
    }

    // ── Persistence ───────────────────────────────────────────────────────────

    pub fn save(&mut self) -> Result<String, String> {
        let Some(path) = &self.file_path else {
            return Err("No file name — use :w <filename>".into());
        };
        let content = self.lines.join("\n") + "\n";
        std::fs::write(path, content).map_err(|e| format!("Error writing: {e}"))?;
        let msg = format!("\"{}\" written", path.display());
        self.modified = false;
        self.refresh_head_lines();
        Ok(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane_with_line(s: &str) -> Pane {
        let mut p = Pane::empty();
        p.lines = vec![s.to_string()];
        p
    }

    #[test]
    fn horizontal_scroll_follows_cursor_to_end_of_line() {
        let mut p = pane_with_line(&"x".repeat(100));
        p.cursor_col = 99; // end of line
        p.scroll_to_cursor(10, 20);
        // Cursor must be visible: viewport_left ..= viewport_left+width-1
        assert!(p.cursor_col >= p.viewport_left);
        assert!(p.cursor_col < p.viewport_left + 20);
        assert_eq!(p.viewport_left, 80); // 99 - 20 + 1
    }

    #[test]
    fn horizontal_scroll_resets_when_moving_to_start() {
        let mut p = pane_with_line(&"x".repeat(100));
        p.cursor_col = 99;
        p.scroll_to_cursor(10, 20);
        assert_eq!(p.viewport_left, 80);

        p.move_line_start();
        p.scroll_to_cursor(10, 20);
        assert_eq!(p.viewport_left, 0);
        assert_eq!(p.cursor_col, 0);
    }

    #[test]
    fn horizontal_scroll_moves_left_with_cursor() {
        let mut p = pane_with_line(&"x".repeat(100));
        p.cursor_col = 50;
        p.viewport_left = 40;
        p.scroll_to_cursor(10, 20);
        // 50 is within [40, 60) — no change
        assert_eq!(p.viewport_left, 40);

        p.cursor_col = 30;
        p.scroll_to_cursor(10, 20);
        assert_eq!(p.viewport_left, 30);
    }

    #[test]
    fn move_line_end_then_scroll_keeps_cursor_visible() {
        let mut p = pane_with_line("hello world this is a long line of text");
        let width = 10;
        p.move_line_end();
        p.scroll_to_cursor(5, width);
        assert_eq!(p.cursor_col, p.current_line_len() - 1);
        assert!(p.cursor_col >= p.viewport_left);
        assert!(p.cursor_col < p.viewport_left + width);
    }
}
