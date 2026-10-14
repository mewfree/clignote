use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::fold::{self, Folds};
use crate::git::{self, LineStatus};
use crate::links;
use crate::structure::{self, StructureOp};
use crate::wrap::{self, WrapState};

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
    /// Wrap geometry from the last render; `None` when not wrapping.
    wrap: Option<WrapState>,
    /// Indent body text under headings (display only), set by the renderer.
    indent: bool,
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

    /// A not-yet-written file at `path` with initial `lines`.
    pub fn new_file(path: PathBuf, lines: Vec<String>) -> Self {
        Self {
            lines,
            file_path: Some(path),
            modified: true,
            ..Self::empty()
        }
    }

    /// Append `line` at the end of the buffer and put the cursor after it.
    pub fn append_line(&mut self, line: String) {
        self.snapshot();
        self.lines.push(line);
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = self.current_line_len();
        self.snap_cursor_visible();
        self.modified = true;
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
            wrap: None,
            indent: false,
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

    // ── Soft wrap & screen layout ─────────────────────────────────────────────

    /// Set by the renderer each frame: `Some` when long lines wrap.
    pub fn set_wrap(&mut self, state: Option<WrapState>) {
        self.wrap = state;
    }

    pub fn is_wrapping(&self) -> bool {
        self.wrap.is_some()
    }

    /// Set by the renderer each frame: indent body text under headings.
    pub fn set_indent(&mut self, on: bool) {
        self.indent = on;
    }

    /// Display-only indent of buffer row `row`: body lines sit one column
    /// deeper than the stars of the heading that owns them. Headings and text
    /// before the first heading are flush left. Capped at half the wrap width
    /// so deep trees never squeeze the text out.
    pub fn row_indent(&self, row: usize) -> usize {
        if !self.indent || fold::heading_level(&self.lines[row]).is_some() {
            return 0;
        }
        let level = self.lines[..row]
            .iter()
            .rev()
            .find_map(|l| fold::heading_level(l))
            .unwrap_or(0);
        let indent = if level == 0 { 0 } else { level + 1 };
        match self.wrap {
            Some(w) => indent.min(w.width / 2),
            None => indent,
        }
    }

    /// Display-cell ranges of the screen rows that buffer row `row` occupies.
    /// A single range when not wrapping.
    pub fn row_segments(&self, row: usize) -> Vec<Range<usize>> {
        let Some(w) = self.wrap else {
            return wrap::whole(self.lines[row].len().max(1));
        };
        // In Insert mode the cursor row shows raw links and the cursor may sit
        // one cell past the end of the text.
        let cursor_cell = w.insert && row == self.cursor_row;
        let mut chars = wrap::display_chars(&self.lines[row], w.conceal_links && !cursor_cell);
        let cells = if cursor_cell {
            chars.len() + 1
        } else {
            chars.len().max(1)
        };
        chars.resize(cells, ' ');
        wrap::segments(&chars, w.width - self.row_indent(row))
    }

    fn row_height(&self, row: usize) -> usize {
        if self.wrap.is_some() {
            self.row_segments(row).len()
        } else {
            1
        }
    }

    /// Index of the screen row of the cursor's buffer row that holds the cursor.
    fn cursor_segment(&self, segs: &[Range<usize>]) -> usize {
        segs.iter()
            .position(|r| self.cursor_col < r.end)
            .unwrap_or(segs.len() - 1)
    }

    /// Screen rows taken by the visible buffer rows in `top..row`.
    fn screen_rows_between(&self, ranges: &[Range<usize>], top: usize, row: usize) -> usize {
        let mut total = 0;
        let mut r = top;
        while r < row {
            total += self.row_height(r);
            match fold::next_visible(ranges, self.lines.len(), r) {
                Some(next) => r = next,
                None => break,
            }
        }
        total
    }

    /// What is on screen, top to bottom, as `(buffer row, wrapped segment)`
    /// pairs, skipping folded rows. Shorter than `height` when the buffer ends.
    pub fn screen_layout(&self, ranges: &[Range<usize>], height: usize) -> Vec<(usize, usize)> {
        let mut out = Vec::with_capacity(height);
        let mut row = self.viewport_top;
        while out.len() < height && row < self.lines.len() {
            let room = height - out.len();
            out.extend((0..self.row_height(row)).take(room).map(|seg| (row, seg)));
            match fold::next_visible(ranges, self.lines.len(), row) {
                Some(next) => row = next,
                None => break,
            }
        }
        out
    }

    /// Buffer position under the content-area cell `(x, y)`, clamped to the text.
    pub fn pos_at_screen(&self, x: usize, y: usize) -> (usize, usize) {
        let ranges = self.fold_ranges();
        let layout = self.screen_layout(&ranges, y + 1);
        let last_row = self.lines.len().saturating_sub(1);
        let &(row, seg) = layout.get(y).or(layout.last()).unwrap_or(&(last_row, 0));
        let indent = self.row_indent(row);
        let col = if self.wrap.is_some() {
            let start = self.row_segments(row).get(seg).map_or(0, |r| r.start);
            start + x.saturating_sub(indent)
        } else {
            // `viewport_left` is a screen offset, so it includes the indent.
            (self.viewport_left + x).saturating_sub(indent)
        };
        let col = col.min(self.lines[row].len().saturating_sub(1));
        (row, col)
    }

    /// Move the cursor one screen row up or down, wrapping-aware. Returns
    /// false at the top/bottom of the buffer.
    fn visual_step(&mut self, down: bool, ranges: &[Range<usize>]) -> bool {
        let segs = self.row_segments(self.cursor_row);
        let seg = self.cursor_segment(&segs);
        let offset = self.cursor_col.saturating_sub(segs[seg].start);

        let (row, target) = if down && seg + 1 < segs.len() {
            (self.cursor_row, segs[seg + 1].clone())
        } else if !down && seg > 0 {
            (self.cursor_row, segs[seg - 1].clone())
        } else {
            let other = if down {
                fold::next_visible(ranges, self.lines.len(), self.cursor_row)
            } else {
                fold::prev_visible(ranges, self.cursor_row)
            };
            let Some(row) = other else {
                return false;
            };
            let mut segs = self.row_segments(row);
            let target = if down {
                segs.swap_remove(0)
            } else {
                segs.pop().unwrap_or(0..0)
            };
            (row, target)
        };
        self.cursor_row = row;
        self.cursor_col = (target.start + offset).min(target.end.saturating_sub(1));
        true
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
                // Every buffer row takes at least one screen row, so at least
                // this many rows must scroll off the top.
                let rows_above = fold::visible_between(&ranges, self.viewport_top, self.cursor_row);
                for _ in 0..(rows_above + 1).saturating_sub(height) {
                    match fold::next_visible(&ranges, self.lines.len(), self.viewport_top) {
                        Some(next) => self.viewport_top = next,
                        None => break,
                    }
                }
                if self.wrap.is_some() {
                    // Wrapped rows are taller: scroll until the cursor's own
                    // screen row fits.
                    let segs = self.row_segments(self.cursor_row);
                    let in_row = self.cursor_segment(&segs);
                    while self.viewport_top < self.cursor_row
                        && self.screen_rows_between(&ranges, self.viewport_top, self.cursor_row)
                            + in_row
                            + 1
                            > height
                    {
                        match fold::next_visible(&ranges, self.lines.len(), self.viewport_top) {
                            Some(next) => self.viewport_top = next,
                            None => break,
                        }
                    }
                }
            }
        }
        if self.wrap.is_some() {
            self.viewport_left = 0;
        } else {
            let col = self.cursor_col + self.row_indent(self.cursor_row);
            scroll_into_view(&mut self.viewport_left, col, width);
        }
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
        if self.wrap.is_some() {
            for _ in 0..n {
                if !self.visual_step(false, &ranges) {
                    break;
                }
            }
            self.clamp_col();
            return;
        }
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
        if self.wrap.is_some() {
            for _ in 0..n {
                if !self.visual_step(true, &ranges) {
                    break;
                }
            }
            self.clamp_col();
            return;
        }
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

    /// Insert a line break at the cursor. On a list item this continues the
    /// list (bullet, next number, fresh checkbox); on an empty item it ends it.
    pub fn insert_newline(&mut self) {
        self.snapshot_insert();
        if let Some(cont) = structure::list_continuation(&self.lines[self.cursor_row]) {
            let line_len = self.lines[self.cursor_row].len();
            if cont.empty && self.cursor_col >= line_len {
                self.lines[self.cursor_row].clear();
                self.cursor_col = 0;
                self.modified = true;
                return;
            }
            if self.cursor_col >= cont.own_len {
                let rest = self.lines[self.cursor_row].split_off(self.cursor_col);
                self.cursor_row += 1;
                self.cursor_col = cont.next_prefix.len();
                self.lines
                    .insert(self.cursor_row, format!("{}{rest}", cont.next_prefix));
                self.folds.lines_inserted(self.cursor_row, 1);
                self.modified = true;
                return;
            }
        }
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
        let prefix = structure::list_continuation(&self.lines[self.cursor_row])
            .filter(|c| !c.empty)
            .map(|c| c.next_prefix)
            .unwrap_or_default();
        let at = self.row_after(self.cursor_row);
        self.cursor_col = prefix.len();
        self.lines.insert(at, prefix);
        self.folds.lines_inserted(at, 1);
        self.cursor_row = at;
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

    // ── Org structure editing ─────────────────────────────────────────────────

    /// Apply a structural command to the heading owning the cursor. Returns a
    /// status message on success, or why it could not be applied.
    pub fn apply_structure(&mut self, op: StructureOp) -> Result<&'static str, &'static str> {
        let row = structure::heading_row(&self.lines, self.cursor_row).ok_or("Not in a heading")?;
        match op {
            StructureOp::Promote | StructureOp::Demote => {
                let promote = op == StructureOp::Promote;
                let new = structure::shift_level(&self.lines[row], promote)
                    .ok_or("Already at top level")?;
                self.snapshot();
                self.lines[row] = new;
                if row == self.cursor_row {
                    self.cursor_col = if promote {
                        self.cursor_col.saturating_sub(1)
                    } else {
                        self.cursor_col + 1
                    };
                }
                self.modified = true;
                Ok(if promote { "Promoted" } else { "Demoted" })
            }
            StructureOp::PromoteSubtree | StructureOp::DemoteSubtree => {
                let promote = op == StructureOp::PromoteSubtree;
                let end = fold::subtree_end(&self.lines, row);
                let shifted: Option<Vec<(usize, String)>> = (row..end)
                    .filter(|&r| fold::heading_level(&self.lines[r]).is_some())
                    .map(|r| structure::shift_level(&self.lines[r], promote).map(|l| (r, l)))
                    .collect();
                let shifted = shifted.ok_or("Already at top level")?;
                self.snapshot();
                for (r, line) in shifted {
                    self.lines[r] = line;
                }
                self.clamp_col();
                self.modified = true;
                Ok(if promote {
                    "Promoted subtree"
                } else {
                    "Demoted subtree"
                })
            }
            StructureOp::MoveUp | StructureOp::MoveDown => {
                let down = op == StructureOp::MoveDown;
                let mv = structure::plan_move(&self.lines, row, down)?;
                self.snapshot();
                let first_len = mv.mid - mv.start;
                self.lines[mv.start..mv.end].rotate_left(first_len);
                self.folds.blocks_swapped(mv.start, mv.mid, mv.end);
                // The cursor travels with the subtree being moved.
                self.cursor_row = if down {
                    self.cursor_row + (mv.end - mv.mid)
                } else {
                    self.cursor_row - (mv.mid - mv.start)
                };
                self.modified = true;
                Ok(if down { "Moved down" } else { "Moved up" })
            }
            StructureOp::TodoNext | StructureOp::TodoPrev => {
                let forward = op == StructureOp::TodoNext;
                let new =
                    structure::cycle_todo(&self.lines[row], forward).ok_or("Not in a heading")?;
                self.snapshot();
                self.lines[row] = new;
                self.clamp_col();
                self.modified = true;
                Ok("TODO state changed")
            }
        }
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
    fn screen_layout_skips_folds_and_click_maps_to_visible_row() {
        let mut p = pane_with_doc(DOC);
        p.cycle_fold();
        let ranges = p.fold_ranges();
        let rows: Vec<usize> = p
            .screen_layout(&ranges, 10)
            .into_iter()
            .map(|(row, _)| row)
            .collect();
        assert_eq!(rows, vec![0, 4, 5]);
        assert_eq!(p.pos_at_screen(0, 1), (4, 0));
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

    #[test]
    fn promote_and_demote_heading_only() {
        let mut p = pane_with_doc("** A\n*** A1\nbody");
        p.cursor_row = 2; // body belongs to A1
        assert_eq!(p.apply_structure(StructureOp::Demote), Ok("Demoted"));
        assert_eq!(p.lines, vec!["** A", "**** A1", "body"]);
        assert_eq!(p.apply_structure(StructureOp::Promote), Ok("Promoted"));
        assert_eq!(p.lines[1], "*** A1");
    }

    #[test]
    fn subtree_promote_refuses_at_top_level() {
        let mut p = pane_with_doc("* A\n** A1");
        assert!(p.apply_structure(StructureOp::PromoteSubtree).is_err());
        assert_eq!(p.lines, vec!["* A", "** A1"]);
        assert!(p.apply_structure(StructureOp::DemoteSubtree).is_ok());
        assert_eq!(p.lines, vec!["** A", "*** A1"]);
    }

    #[test]
    fn move_subtree_down_and_up_keeps_cursor_on_it() {
        let mut p = pane_with_doc("* A\na\n* B\nb");
        p.cursor_row = 1; // in A
        assert_eq!(p.apply_structure(StructureOp::MoveDown), Ok("Moved down"));
        assert_eq!(p.lines, vec!["* B", "b", "* A", "a"]);
        assert_eq!(p.cursor_row, 3);
        assert_eq!(p.apply_structure(StructureOp::MoveUp), Ok("Moved up"));
        assert_eq!(p.lines, vec!["* A", "a", "* B", "b"]);
        assert_eq!(p.cursor_row, 1);
        assert!(p.apply_structure(StructureOp::MoveUp).is_err());
    }

    #[test]
    fn moving_a_subtree_carries_its_fold() {
        let mut p = pane_with_doc("* A\na\n* B\nb");
        p.cycle_fold(); // fold A
        p.apply_structure(StructureOp::MoveDown).unwrap();
        assert_eq!(p.fold_ranges(), vec![3..4]); // A now at row 2
    }

    #[test]
    fn todo_cycle_on_heading() {
        let mut p = pane_with_doc("* Task\nbody");
        p.cursor_row = 1;
        p.apply_structure(StructureOp::TodoNext).unwrap();
        assert_eq!(p.lines[0], "* TODO Task");
        p.apply_structure(StructureOp::TodoPrev).unwrap();
        assert_eq!(p.lines[0], "* Task");
    }

    #[test]
    fn structure_outside_heading_is_an_error() {
        let mut p = pane_with_doc("preamble\n* A");
        assert_eq!(
            p.apply_structure(StructureOp::Promote),
            Err("Not in a heading")
        );
    }

    #[test]
    fn enter_continues_list_items() {
        let mut p = pane_with_doc("- [X] first");
        p.cursor_col = p.lines[0].len();
        p.insert_newline();
        assert_eq!(p.lines, vec!["- [X] first", "- [ ] "]);
        assert_eq!((p.cursor_row, p.cursor_col), (1, 6));
        // Enter on the now-empty item ends the list.
        p.insert_newline();
        assert_eq!(p.lines, vec!["- [X] first", ""]);
    }

    #[test]
    fn enter_numbers_ordered_lists_and_splits_text() {
        let mut p = pane_with_doc("1. ab");
        p.cursor_col = 4; // between a and b
        p.insert_newline();
        assert_eq!(p.lines, vec!["1. a", "2. b"]);
    }

    #[test]
    fn open_line_below_continues_list() {
        let mut p = pane_with_doc("  - item");
        p.open_line_below();
        assert_eq!(p.lines[1], "  - ");
        assert_eq!(p.cursor_col, 4);
    }

    fn wrapped(s: &str, width: usize) -> Pane {
        let mut p = pane_with_doc(s);
        p.set_wrap(Some(WrapState {
            width,
            conceal_links: true,
            insert: false,
        }));
        p
    }

    // "aaaa bbbb cccc" wraps at width 10 into "aaaa bbbb " / "cccc".
    const LONG: &str = "aaaa bbbb cccc";

    #[test]
    fn wrapped_row_takes_several_screen_rows() {
        let p = wrapped(&format!("{LONG}\nshort"), 10);
        assert_eq!(p.row_segments(0), vec![0..10, 10..14]);
        assert_eq!(p.screen_layout(&[], 10), vec![(0, 0), (0, 1), (1, 0)]);
    }

    #[test]
    fn j_and_k_move_by_screen_row_keeping_column_offset() {
        let mut p = wrapped(&format!("{LONG}\nshort"), 10);
        p.cursor_col = 2;
        p.move_down(1);
        assert_eq!((p.cursor_row, p.cursor_col), (0, 12)); // 2nd segment
        p.move_down(1);
        assert_eq!((p.cursor_row, p.cursor_col), (1, 2)); // next buffer row
        p.move_up(1);
        assert_eq!((p.cursor_row, p.cursor_col), (0, 12));
        p.move_up(1);
        assert_eq!((p.cursor_row, p.cursor_col), (0, 2));
    }

    #[test]
    fn unwrapped_j_still_moves_by_buffer_row() {
        let mut p = pane_with_doc(&format!("{LONG}\nshort"));
        p.move_down(1);
        assert_eq!(p.cursor_row, 1);
    }

    #[test]
    fn scrolling_accounts_for_wrapped_heights() {
        // Three 2-row lines in a 4-row window: cursor on the 3rd forces a scroll.
        let mut p = wrapped(&format!("{LONG}\n{LONG}\n{LONG}"), 10);
        p.cursor_row = 2;
        p.scroll_to_cursor(4, 10);
        assert_eq!(p.viewport_top, 1);
        p.cursor_row = 0;
        p.scroll_to_cursor(4, 10);
        assert_eq!(p.viewport_top, 0);
    }

    #[test]
    fn click_maps_screen_row_to_wrapped_segment() {
        let p = wrapped(&format!("{LONG}\nshort"), 10);
        assert_eq!(p.pos_at_screen(3, 0), (0, 3));
        assert_eq!(p.pos_at_screen(2, 1), (0, 12)); // continuation row
        assert_eq!(p.pos_at_screen(1, 2), (1, 1));
    }

    #[test]
    fn insert_cursor_cell_can_add_a_screen_row() {
        let mut p = wrapped("0123456789", 10); // exactly one full row
        assert_eq!(p.row_segments(0).len(), 1);
        p.set_wrap(Some(WrapState {
            width: 10,
            conceal_links: true,
            insert: true,
        }));
        assert_eq!(p.row_segments(0).len(), 2); // cursor cell wraps
    }

    fn indented(s: &str, width: usize) -> Pane {
        let mut p = wrapped(s, width);
        p.set_indent(true);
        p
    }

    #[test]
    fn body_is_indented_by_heading_level() {
        let p = indented("intro\n* one\nbody\n** two\ndeep", 40);
        let got: Vec<usize> = (0..5).map(|r| p.row_indent(r)).collect();
        assert_eq!(got, vec![0, 0, 2, 0, 3]);
    }

    #[test]
    fn indent_off_means_no_indent() {
        let mut p = indented("* one\nbody", 40);
        p.set_indent(false);
        assert_eq!(p.row_indent(1), 0);
    }

    #[test]
    fn indent_narrows_wrap_width() {
        // Indent 2 leaves 8 cells: "aaaa " / "bbbb " / "cccc".
        let p = indented("* h\naaaa bbbb cccc", 10);
        assert_eq!(p.row_segments(1), vec![0..5, 5..10, 10..14]);
    }

    #[test]
    fn indent_is_capped_at_half_the_width() {
        let p = indented("********* h\nbody", 6);
        assert_eq!(p.row_indent(1), 3);
    }

    #[test]
    fn click_accounts_for_indent() {
        let p = indented("* h\nbody", 40);
        assert_eq!(p.pos_at_screen(2, 1), (1, 0));
        assert_eq!(p.pos_at_screen(4, 1), (1, 2));
        assert_eq!(p.pos_at_screen(0, 1), (1, 0)); // inside the indent
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
