//! Heading folding (org `TAB` / `S-TAB` visibility cycling).
//!
//! Folds are stored as marks on heading rows. Whether a row is hidden is
//! derived from the marks and the buffer on demand, so the buffer stays the
//! single source of truth; edits only need to tell [`Folds`] where lines were
//! inserted or removed.

use std::collections::BTreeMap;
use std::ops::Range;

/// What a folded heading hides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldKind {
    /// Everything up to the next heading of the same or higher level.
    Subtree,
    /// Only the body text up to the next heading of any level.
    Body,
}

/// State of the global `S-TAB` cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlobalCycle {
    #[default]
    ShowAll,
    Overview,
    Contents,
}

impl GlobalCycle {
    pub fn label(self) -> &'static str {
        match self {
            GlobalCycle::ShowAll => "SHOW ALL",
            GlobalCycle::Overview => "OVERVIEW",
            GlobalCycle::Contents => "CONTENTS",
        }
    }
}

/// Heading level (`*` count) if `line` is an org headline.
pub fn heading_level(line: &str) -> Option<usize> {
    let stars = line.bytes().take_while(|&b| b == b'*').count();
    (stars > 0 && line.as_bytes().get(stars) == Some(&b' ')).then_some(stars)
}

/// Exclusive end row of the subtree headed at `row`.
pub fn subtree_end(lines: &[String], row: usize) -> usize {
    let Some(level) = lines.get(row).and_then(|l| heading_level(l)) else {
        return row + 1;
    };
    lines[row + 1..]
        .iter()
        .position(|l| heading_level(l).is_some_and(|lv| lv <= level))
        .map_or(lines.len(), |p| row + 1 + p)
}

/// Exclusive end row of the body text under the heading at `row`.
pub fn body_end(lines: &[String], row: usize) -> usize {
    lines[row + 1..]
        .iter()
        .position(|l| heading_level(l).is_some())
        .map_or(lines.len(), |p| row + 1 + p)
}

/// The hidden range containing `row`, if any. `ranges` must be sorted and
/// disjoint, as returned by [`Folds::ranges`].
pub fn range_containing(ranges: &[Range<usize>], row: usize) -> Option<&Range<usize>> {
    let i = ranges.partition_point(|r| r.end <= row);
    ranges.get(i).filter(|r| r.start <= row)
}

/// Next visible row after `row`, or `None` at the end of the buffer.
pub fn next_visible(ranges: &[Range<usize>], len: usize, row: usize) -> Option<usize> {
    let next = row + 1;
    let next = range_containing(ranges, next).map_or(next, |r| r.end);
    (next < len).then_some(next)
}

/// Previous visible row before `row`, or `None` at the top.
pub fn prev_visible(ranges: &[Range<usize>], row: usize) -> Option<usize> {
    let prev = row.checked_sub(1)?;
    Some(range_containing(ranges, prev).map_or(prev, |r| r.start - 1))
}

/// Number of visible rows in `top..row`.
pub fn visible_between(ranges: &[Range<usize>], top: usize, row: usize) -> usize {
    let hidden: usize = ranges
        .iter()
        .map(|r| r.end.min(row).saturating_sub(r.start.max(top)))
        .sum();
    row.saturating_sub(top) - hidden
}

#[derive(Debug, Clone, Default)]
pub struct Folds {
    marks: BTreeMap<usize, FoldKind>,
    cycle: GlobalCycle,
}

impl Folds {
    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }

    /// Sorted, disjoint row ranges currently hidden. Marks on rows that are no
    /// longer headings (or fold nothing) are ignored.
    pub fn ranges(&self, lines: &[String]) -> Vec<Range<usize>> {
        let mut out: Vec<Range<usize>> = Vec::new();
        for (&row, &kind) in &self.marks {
            let Some(line) = lines.get(row) else {
                continue;
            };
            if heading_level(line).is_none() {
                continue;
            }
            let end = match kind {
                FoldKind::Subtree => subtree_end(lines, row),
                FoldKind::Body => body_end(lines, row),
            };
            let range = row + 1..end;
            // Folds nested inside an outer fold are already hidden.
            if range.is_empty() || out.last().is_some_and(|last| range.start < last.end) {
                continue;
            }
            out.push(range);
        }
        out
    }

    /// Account for `n` lines inserted at row `at`.
    pub fn lines_inserted(&mut self, at: usize, n: usize) {
        if self.marks.is_empty() {
            return;
        }
        self.marks = std::mem::take(&mut self.marks)
            .into_iter()
            .map(|(r, k)| (if r >= at { r + n } else { r }, k))
            .collect();
    }

    /// Account for `n` lines removed starting at row `at`.
    pub fn lines_removed(&mut self, at: usize, n: usize) {
        if self.marks.is_empty() {
            return;
        }
        self.marks = std::mem::take(&mut self.marks)
            .into_iter()
            .filter(|(r, _)| !(at..at + n).contains(r))
            .map(|(r, k)| (if r >= at + n { r - n } else { r }, k))
            .collect();
    }

    pub fn clear(&mut self) {
        self.marks.clear();
    }

    /// Remove every fold hiding `row` (used to reveal search hits).
    pub fn reveal(&mut self, lines: &[String], row: usize) {
        self.marks.retain(|&r, &mut kind| {
            let end = match kind {
                FoldKind::Subtree => subtree_end(lines, r),
                FoldKind::Body => body_end(lines, r),
            };
            !(r < row && row < end)
        });
    }

    pub fn is_folded(&self, row: usize) -> bool {
        self.marks.contains_key(&row)
    }

    /// Cycle visibility of the heading at `row`:
    /// FOLDED → CHILDREN → SUBTREE → FOLDED. Returns the new state's name, or
    /// `None` if `row` isn't a heading with anything under it.
    pub fn cycle_heading(&mut self, lines: &[String], row: usize) -> Option<&'static str> {
        heading_level(lines.get(row)?)?;
        let end = subtree_end(lines, row);
        if end == row + 1 {
            return None;
        }
        if self.marks.contains_key(&row) {
            self.marks.remove(&row);
            let mut child_level = None;
            for r in row + 1..end {
                let Some(level) = heading_level(&lines[r]) else {
                    continue;
                };
                if *child_level.get_or_insert(level) != level {
                    continue;
                }
                if subtree_end(lines, r) > r + 1 {
                    self.marks.insert(r, FoldKind::Subtree);
                }
            }
            Some("CHILDREN")
        } else if self.marks.range(row + 1..end).next().is_some() {
            let inner: Vec<usize> = self.marks.range(row + 1..end).map(|(&r, _)| r).collect();
            for r in inner {
                self.marks.remove(&r);
            }
            Some("SUBTREE")
        } else {
            self.marks.insert(row, FoldKind::Subtree);
            Some("FOLDED")
        }
    }

    /// Advance the global cycle: SHOW ALL → OVERVIEW → CONTENTS → SHOW ALL.
    /// Returns `None` when the buffer has no headings.
    pub fn cycle_global(&mut self, lines: &[String]) -> Option<GlobalCycle> {
        let top_level = lines.iter().filter_map(|l| heading_level(l)).min()?;
        let next = match self.cycle {
            GlobalCycle::ShowAll => GlobalCycle::Overview,
            GlobalCycle::Overview => GlobalCycle::Contents,
            GlobalCycle::Contents => GlobalCycle::ShowAll,
        };
        self.marks.clear();
        for (row, line) in lines.iter().enumerate() {
            let Some(level) = heading_level(line) else {
                continue;
            };
            match next {
                GlobalCycle::Overview
                    if level == top_level && subtree_end(lines, row) > row + 1 =>
                {
                    self.marks.insert(row, FoldKind::Subtree);
                }
                GlobalCycle::Contents if body_end(lines, row) > row + 1 => {
                    self.marks.insert(row, FoldKind::Body);
                }
                _ => {}
            }
        }
        self.cycle = next;
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> Vec<String> {
        s.lines().map(str::to_owned).collect()
    }

    const SAMPLE: &str = "preamble\n* A\ntext a\n** A1\ntext a1\n** A2\n* B\ntext b";

    #[test]
    fn heading_level_requires_space() {
        assert_eq!(heading_level("** x"), Some(2));
        assert_eq!(heading_level("**x"), None);
        assert_eq!(heading_level("text"), None);
    }

    #[test]
    fn subtree_and_body_extents() {
        let l = doc(SAMPLE);
        assert_eq!(subtree_end(&l, 1), 6); // * A through A2
        assert_eq!(body_end(&l, 1), 3); // stops at ** A1
        assert_eq!(subtree_end(&l, 6), 8); // * B to EOF
    }

    #[test]
    fn fold_hides_subtree() {
        let l = doc(SAMPLE);
        let mut f = Folds::default();
        assert_eq!(f.cycle_heading(&l, 1), Some("FOLDED"));
        let ranges = f.ranges(&l);
        assert_eq!(ranges, vec![2..6]);
        assert_eq!(next_visible(&ranges, l.len(), 1), Some(6));
        assert_eq!(prev_visible(&ranges, 6), Some(1));
        assert_eq!(visible_between(&ranges, 0, 7), 3); // preamble, * A, * B
    }

    #[test]
    fn heading_cycle_folded_children_subtree() {
        let l = doc(SAMPLE);
        let mut f = Folds::default();
        assert_eq!(f.cycle_heading(&l, 1), Some("FOLDED"));
        assert_eq!(f.cycle_heading(&l, 1), Some("CHILDREN"));
        // A1 has a body so it is folded; A2 is a leaf so it is not.
        assert_eq!(f.ranges(&l), vec![4..5]);
        assert_eq!(f.cycle_heading(&l, 1), Some("SUBTREE"));
        assert!(f.ranges(&l).is_empty());
        assert_eq!(f.cycle_heading(&l, 1), Some("FOLDED"));
    }

    #[test]
    fn leaf_heading_does_not_cycle() {
        let l = doc(SAMPLE);
        assert_eq!(Folds::default().cycle_heading(&l, 5), None);
        assert_eq!(Folds::default().cycle_heading(&l, 0), None);
    }

    #[test]
    fn global_cycle() {
        let l = doc(SAMPLE);
        let mut f = Folds::default();
        assert_eq!(f.cycle_global(&l), Some(GlobalCycle::Overview));
        assert_eq!(f.ranges(&l), vec![2..6, 7..8]);
        assert_eq!(f.cycle_global(&l), Some(GlobalCycle::Contents));
        // bodies hidden, sub-headings still visible
        assert_eq!(f.ranges(&l), vec![2..3, 4..5, 7..8]);
        assert_eq!(f.cycle_global(&l), Some(GlobalCycle::ShowAll));
        assert!(f.ranges(&l).is_empty());
    }

    #[test]
    fn marks_follow_line_edits() {
        let l = doc(SAMPLE);
        let mut f = Folds::default();
        f.cycle_heading(&l, 6); // fold * B
        f.lines_inserted(0, 2);
        assert!(f.is_folded(8));
        f.lines_removed(1, 2);
        assert!(f.is_folded(6));
        f.lines_removed(6, 1);
        assert!(f.is_empty());
    }

    #[test]
    fn reveal_unfolds_owner() {
        let l = doc(SAMPLE);
        let mut f = Folds::default();
        f.cycle_heading(&l, 1);
        f.reveal(&l, 4);
        assert!(f.ranges(&l).is_empty());
    }
}
