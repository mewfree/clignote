use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::fold::{self, Folds};
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

/// Buffer state saved for undo/redo: lines, cursor row, cursor col.
type Snapshot = (Vec<String>, usize, usize);

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
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    /// True while an insert session has already recorded its undo snapshot,
    /// so a run of typing undoes as one step.
    insert_open: bool,
    folds: Folds,
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
            redo_stack: Vec::new(),
            insert_open: false,
            folds: Folds::default(),
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

    /// Row ranges currently hidden by folds (sorted, disjoint).
    pub fn fold_ranges(&self) -> Vec<Range<usize>> {
        self.folds.ranges(&self.lines)
    }

    /// Whether the heading at `row` is folded over a non-empty range.
    pub fn is_fold_start(ranges: &[Range<usize>], row: usize) -> bool {
        ranges.binary_search_by_key(&(row + 1), |r| r.start).is_ok()
    }

    /// Buffer rows shown on screen, top to bottom, skipping folded rows.
    /// Shorter than `height` when the buffer ends first.
    pub fn visible_rows(&self, ranges: &[Range<usize>], height: usize) -> Vec<usize> {
        let mut rows = Vec::with_capacity(height);
        let mut row = self.viewport_top;
        while rows.len() < height && row < self.lines.len() {
            rows.push(row);
            match fold::next_visible(ranges, self.lines.len(), row) {
                Some(next) => row = next,
                None => break,
            }
        }
        rows
    }

    /// Buffer row displayed `offset` visible rows below the viewport top.
    pub fn row_at_screen_offset(&self, offset: usize) -> usize {
        let ranges = self.fold_ranges();
        let mut row = self.viewport_top.min(self.lines.len().saturating_sub(1));
        for _ in 0..offset {
            match fold::next_visible(&ranges, self.lines.len(), row) {
                Some(next) => row = next,
                None => break,
            }
        }
        row
    }

    /// Keep the cursor inside the visible viewport (vertical and horizontal).
    pub fn scroll_to_cursor(&mut self, height: usize, width: usize) {
        let ranges = self.fold_ranges();
        // The viewport may start inside a freshly folded region.
        if let Some(r) = fold::range_containing(&ranges, self.viewport_top) {
            self.viewport_top = r.start - 1;
        }
        if height > 0 {
            if self.cursor_row < self.viewport_top {
                self.viewport_top = self.cursor_row;
            } else {
                let on_screen = fold::visible_between(&ranges, self.viewport_top, self.cursor_row);
                for _ in 0..(on_screen + 1).saturating_sub(height) {
                    match fold::next_visible(&ranges, self.lines.len(), self.viewport_top) {
                        Some(next) => self.viewport_top = next,
                        None => break,
                    }
                }
            }
        }
        scroll_into_view(&mut self.viewport_left, self.cursor_col, width);
    }

    pub fn scroll_down(&mut self, n: usize) {
        let ranges = self.fold_ranges();
        for _ in 0..n {
            match fold::next_visible(&ranges, self.lines.len(), self.viewport_top) {
                Some(next) => self.viewport_top = next,
                None => break,
            }
        }
        self.cursor_row = self.cursor_row.max(self.viewport_top);
        if self.cursor_row >= self.lines.len() {
            self.cursor_row = self.lines.len().saturating_sub(1);
        }
        self.clamp_col();
    }

    pub fn scroll_up(&mut self, n: usize) {
        let ranges = self.fold_ranges();
        for _ in 0..n {
            match fold::prev_visible(&ranges, self.viewport_top) {
                Some(prev) => self.viewport_top = prev,
                None => break,
            }
        }
        self.clamp_col();
    }

    // ── Folding ───────────────────────────────────────────────────────────────

    /// If the cursor ended up inside a folded region, move it to the heading.
    fn snap_cursor_visible(&mut self) {
        let ranges = self.fold_ranges();
        if let Some(r) = fold::range_containing(&ranges, self.cursor_row) {
            self.cursor_row = r.start - 1;
            self.clamp_col();
        }
    }

    /// `TAB` on a heading: cycle FOLDED → CHILDREN → SUBTREE. Returns a
    /// status message.
    pub fn cycle_fold(&mut self) -> String {
        match self.folds.cycle_heading(&self.lines, self.cursor_row) {
            Some(state) => {
                self.snap_cursor_visible();
                state.to_string()
            }
            None => "Nothing to fold here".to_string(),
        }
    }

    /// `S-TAB`: cycle the whole buffer OVERVIEW → CONTENTS → SHOW ALL.
    pub fn cycle_global_fold(&mut self) -> String {
        match self.folds.cycle_global(&self.lines) {
            Some(state) => {
                self.snap_cursor_visible();
                state.label().to_string()
            }
            None => "No headings".to_string(),
        }
    }

    /// Unfold whatever hides `row`, e.g. when jumping to a search match.
    pub fn reveal(&mut self, row: usize) {
        self.folds.reveal(&self.lines, row);
    }

    /// Row where a line opened "after" `row` goes: past the hidden body when
    /// `row` is a folded heading.
    fn row_after(&self, row: usize) -> usize {
        let ranges = self.fold_ranges();
        fold::range_containing(&ranges, row + 1).map_or(row + 1, |r| r.end)
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

    /// Move up `n` visible rows (folded regions count as their heading).
    pub fn move_up(&mut self, n: usize) {
        let ranges = self.fold_ranges();
        for _ in 0..n {
            match fold::prev_visible(&ranges, self.cursor_row) {
                Some(prev) => self.cursor_row = prev,
                None => break,
            }
        }
        self.clamp_col();
    }

    /// Move down `n` visible rows (folded regions count as their heading).
    pub fn move_down(&mut self, n: usize) {
        let ranges = self.fold_ranges();
        for _ in 0..n {
            match fold::next_visible(&ranges, self.lines.len(), self.cursor_row) {
                Some(next) => self.cursor_row = next,
                None => break,
            }
        }
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
        self.snap_cursor_visible();
        self.clamp_col();
    }

    // ── Undo ──────────────────────────────────────────────────────────────────

    fn current_snapshot(&self) -> Snapshot {
        (self.lines.clone(), self.cursor_row, self.cursor_col)
    }

    fn restore(&mut self, (lines, row, col): Snapshot) {
        if lines.len() != self.lines.len() {
            self.folds.clear();
        }
        self.lines = lines;
        self.cursor_row = row;
        self.cursor_col = col;
        self.modified = true;
    }

    /// Record the state before a discrete edit. Invalidates redo history.
    fn snapshot(&mut self) {
        let snap = self.current_snapshot();
        self.undo_stack.push(snap);
        self.redo_stack.clear();
        self.insert_open = false;
    }

    /// Snapshot for an edit made while inserting: only the first edit of an
    /// insert session records one, so typing a word is a single undo step.
    fn snapshot_insert(&mut self) {
        if !self.insert_open {
            self.snapshot();
            self.insert_open = true;
        }
    }

    /// Close the current insert session; the next insert starts a new undo step.
    pub fn end_insert(&mut self) {
        self.insert_open = false;
    }

    pub fn undo(&mut self) -> bool {
        self.end_insert();
        let Some(prev) = self.undo_stack.pop() else {
            return false;
        };
        let current = self.current_snapshot();
        self.redo_stack.push(current);
        self.restore(prev);
        true
    }

    pub fn redo(&mut self) -> bool {
        self.end_insert();
        let Some(next) = self.redo_stack.pop() else {
            return false;
        };
        let current = self.current_snapshot();
        self.undo_stack.push(current);
        self.restore(next);
        true
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
        self.folds.lines_removed(self.cursor_row, 1);
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
        self.folds.lines_removed(lo, hi - lo + 1);
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
        self.folds.lines_removed(sr + 1, er - sr);
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
        let insert_at = self.row_after(self.cursor_row);
        self.lines
            .splice(insert_at..insert_at, lines.iter().cloned());
        self.folds.lines_inserted(insert_at, lines.len());
        self.cursor_row = insert_at;
        self.modified = true;
    }

    pub fn paste_lines_before(&mut self, lines: &[String]) {
        self.snapshot();
        let at = self.cursor_row;
        self.lines.splice(at..at, lines.iter().cloned());
        self.folds.lines_inserted(at, lines.len());
        self.modified = true;
    }

    pub fn insert_char(&mut self, c: char) {
        self.snapshot_insert();
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
        self.snapshot_insert();
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
            self.lines[self.cursor_row].remove(self.cursor_col);
            self.modified = true;
        } else {
            let current = self.lines.remove(self.cursor_row);
            self.folds.lines_removed(self.cursor_row, 1);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
            self.lines[self.cursor_row].push_str(&current);
            self.modified = true;
        }
    }

    pub fn insert_newline(&mut self) {
        self.snapshot_insert();
        let rest = self.lines[self.cursor_row].split_off(self.cursor_col);
        self.cursor_row += 1;
        self.lines.insert(self.cursor_row, rest);
        self.folds.lines_inserted(self.cursor_row, 1);
        self.cursor_col = 0;
        self.modified = true;
    }

    pub fn open_line_below(&mut self) {
        self.snapshot();
        self.insert_open = true;
        let at = self.row_after(self.cursor_row);
        self.lines.insert(at, String::new());
        self.folds.lines_inserted(at, 1);
        self.cursor_row = at;
        self.cursor_col = 0;
        self.modified = true;
    }

    pub fn open_line_above(&mut self) {
        self.snapshot();
        self.insert_open = true;
        self.folds.lines_inserted(self.cursor_row, 1);
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

    fn pane_with_doc(s: &str) -> Pane {
        let mut p = Pane::empty();
        p.lines = s.lines().map(str::to_owned).collect();
        p
    }

    const DOC: &str = "* A\na body\n** A1\nx\n* B\nb body";

    #[test]
    fn folded_rows_are_skipped_by_movement() {
        let mut p = pane_with_doc(DOC);
        assert_eq!(p.cycle_fold(), "FOLDED");
        p.move_down(1);
        assert_eq!(p.cursor_row, 4); // straight to * B
        p.move_up(1);
        assert_eq!(p.cursor_row, 0);
        p.move_file_end();
        assert_eq!(p.cursor_row, 5);
    }

    #[test]
    fn visible_rows_skip_folds_and_screen_offset_matches() {
        let mut p = pane_with_doc(DOC);
        p.cycle_fold();
        let ranges = p.fold_ranges();
        assert_eq!(p.visible_rows(&ranges, 10), vec![0, 4, 5]);
        assert_eq!(p.row_at_screen_offset(1), 4);
    }

    #[test]
    fn global_fold_moves_cursor_to_heading() {
        let mut p = pane_with_doc(DOC);
        p.cursor_row = 3;
        assert_eq!(p.cycle_global_fold(), "OVERVIEW");
        assert_eq!(p.cursor_row, 0); // was inside * A, snapped to its heading
    }

    #[test]
    fn folds_follow_edits_above_them() {
        let mut p = pane_with_doc(DOC);
        p.cursor_row = 4;
        p.cycle_fold(); // fold * B
        p.cursor_row = 0;
        p.open_line_above();
        p.cursor_row = 5;
        assert_eq!(p.lines[5], "* B");
        let ranges = p.fold_ranges();
        assert_eq!(ranges, vec![6..7]);
    }

    #[test]
    fn open_line_below_folded_heading_goes_after_the_fold() {
        let mut p = pane_with_doc(DOC);
        p.cycle_fold();
        p.open_line_below();
        assert_eq!(p.cursor_row, 4);
        assert_eq!(p.lines[4], "");
        assert_eq!(p.lines[5], "* B");
    }

    #[test]
    fn undo_that_changes_line_count_clears_folds() {
        let mut p = pane_with_doc(DOC);
        p.cursor_row = 5;
        p.open_line_below();
        p.cursor_row = 0;
        p.cycle_fold();
        p.undo();
        assert!(p.fold_ranges().is_empty());
    }

    fn type_str(p: &mut Pane, s: &str) {
        s.chars().for_each(|c| p.insert_char(c));
    }

    #[test]
    fn typing_a_word_is_one_undo_step() {
        let mut p = Pane::empty();
        type_str(&mut p, "hello");
        assert_eq!(p.lines[0], "hello");
        assert!(p.undo());
        assert_eq!(p.lines[0], "");
        assert!(!p.undo());
    }

    #[test]
    fn ending_insert_starts_new_undo_step() {
        let mut p = Pane::empty();
        type_str(&mut p, "ab");
        p.end_insert();
        type_str(&mut p, "cd");
        p.undo();
        assert_eq!(p.lines[0], "ab");
    }

    #[test]
    fn open_line_and_typing_undo_together() {
        let mut p = pane_with_line("x");
        p.open_line_below();
        type_str(&mut p, "new");
        p.undo();
        assert_eq!(p.lines, vec!["x"]);
    }

    #[test]
    fn redo_restores_undone_change_and_new_edit_clears_it() {
        let mut p = Pane::empty();
        type_str(&mut p, "abc");
        p.undo();
        assert!(p.redo());
        assert_eq!(p.lines[0], "abc");
        assert!(!p.redo());

        p.undo();
        p.end_insert();
        type_str(&mut p, "z");
        assert!(!p.redo());
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
