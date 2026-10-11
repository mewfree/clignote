//! Org structural editing on raw buffer lines: heading promote/demote,
//! subtree moves, TODO keyword cycling and list-item continuation.
//!
//! Everything here is a pure function over `&str` / `&[String]` so it can be
//! unit-tested without a pane; `Pane` applies the results to the buffer.

use crate::fold::{heading_level, subtree_end};

/// A structural editing command (bound to `M-h/l/j/k`, `S-←/→` and `SPC m …`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructureOp {
    /// Promote the heading (fewer stars); children are left alone.
    Promote,
    /// Demote the heading (more stars); children are left alone.
    Demote,
    PromoteSubtree,
    DemoteSubtree,
    /// Swap the subtree with the previous sibling.
    MoveUp,
    /// Swap the subtree with the next sibling.
    MoveDown,
    TodoNext,
    TodoPrev,
}

/// Row of the heading owning `row`: the nearest headline at or above it.
pub fn heading_row(lines: &[String], row: usize) -> Option<usize> {
    let end = (row + 1).min(lines.len());
    lines[..end]
        .iter()
        .rposition(|l| heading_level(l).is_some())
}

/// Result of moving a subtree: the rotated range and where its blocks went.
#[derive(Debug, PartialEq, Eq)]
pub struct SubtreeMove {
    /// Start of the first block (the moved range is `start..end`).
    pub start: usize,
    /// Boundary between the two swapped blocks (before the swap).
    pub mid: usize,
    pub end: usize,
}

/// Find the two adjacent same-level subtrees to swap when moving the subtree
/// at `row` down (`down = true`) or up.
pub fn plan_move(lines: &[String], row: usize, down: bool) -> Result<SubtreeMove, &'static str> {
    let level = heading_level(&lines[row]).ok_or("Not in a heading")?;
    let end = subtree_end(lines, row);
    if down {
        match lines.get(end).and_then(|l| heading_level(l)) {
            Some(l) if l == level => Ok(SubtreeMove {
                start: row,
                mid: end,
                end: subtree_end(lines, end),
            }),
            _ => Err("Cannot move down any further"),
        }
    } else {
        // Previous sibling: nearest earlier heading of the same level, with no
        // shallower heading in between.
        for r in (0..row).rev() {
            match heading_level(&lines[r]) {
                Some(l) if l == level => {
                    return Ok(SubtreeMove {
                        start: r,
                        mid: row,
                        end,
                    })
                }
                Some(l) if l < level => break,
                _ => {}
            }
        }
        Err("Cannot move up any further")
    }
}

/// Add or remove one star on a headline. Returns `None` if `line` is not a
/// headline, or if promoting a top-level one.
pub fn shift_level(line: &str, promote: bool) -> Option<String> {
    let level = heading_level(line)?;
    if promote {
        (level > 1).then(|| line[1..].to_string())
    } else {
        Some(format!("*{line}"))
    }
}

const TODO_KEYWORDS: &[&str] = &[
    "TODO",
    "NEXT",
    "DOING",
    "WAITING",
    "HOLD",
    "DONE",
    "CANCELLED",
];

/// Cycle the TODO keyword of a headline: (none) → TODO → DONE → (none),
/// or the reverse. Other known keywords count as "open" (TODO-like).
pub fn cycle_todo(line: &str, forward: bool) -> Option<String> {
    let level = heading_level(line)?;
    let (stars, rest) = line.split_at(level);
    let rest = rest.trim_start_matches(' ');
    let first = rest.split(' ').next().unwrap_or("");
    let (current, title) = if TODO_KEYWORDS.contains(&first) {
        (Some(first), rest[first.len()..].trim_start_matches(' '))
    } else {
        (None, rest)
    };
    let index = match current {
        None => 0,
        Some("DONE") => 2,
        Some(_) => 1,
    };
    let next = (index + if forward { 1 } else { 2 }) % 3;
    let keyword = ["", "TODO", "DONE"][next];
    let mut out = String::from(stars);
    for part in [keyword, title] {
        if !part.is_empty() {
            out.push(' ');
            out.push_str(part);
        }
    }
    if out.len() == stars.len() {
        out.push(' ');
    }
    Some(out)
}

/// How a list item continues onto a new line.
#[derive(Debug, PartialEq, Eq)]
pub struct ListContinuation {
    /// Byte length of this line's own prefix (indent + bullet + checkbox).
    pub own_len: usize,
    /// Prefix for the next item (`"  - [ ] "`, `"2. "`, …).
    pub next_prefix: String,
    /// The item has no text yet, so Enter should end the list.
    pub empty: bool,
}

/// Parse `line` as a list item (`-`, `+`, `N.`), if it is one.
pub fn list_continuation(line: &str) -> Option<ListContinuation> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let tail = &line[indent..];

    let (bullet_len, next_bullet) = if tail.starts_with("- ") || tail.starts_with("+ ") {
        (2, tail[..1].to_string())
    } else {
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !tail[digits..].starts_with(". ") {
            return None;
        }
        let n: u64 = tail[..digits].parse().ok()?;
        (digits + 2, format!("{}.", n + 1))
    };

    let after = &tail[bullet_len..];
    let checkbox = after.len() >= 3
        && after.starts_with('[')
        && after[1..].starts_with([' ', 'X', 'x', '-'])
        && after[2..].starts_with(']')
        && (after.len() == 3 || after[3..].starts_with(' '));
    let cb_len = if checkbox {
        // "[ ]" plus its trailing space, when present.
        if after.len() > 3 {
            4
        } else {
            3
        }
    } else {
        0
    };

    let own_len = indent + bullet_len + cb_len;
    let mut next_prefix = format!("{}{} ", " ".repeat(indent), next_bullet);
    if checkbox {
        next_prefix.push_str("[ ] ");
    }
    Some(ListContinuation {
        own_len,
        next_prefix,
        empty: line[own_len..].trim().is_empty(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> Vec<String> {
        s.lines().map(str::to_owned).collect()
    }

    #[test]
    fn heading_row_finds_owner() {
        let l = doc("pre\n* A\nbody\n** B\nx");
        assert_eq!(heading_row(&l, 0), None);
        assert_eq!(heading_row(&l, 2), Some(1));
        assert_eq!(heading_row(&l, 4), Some(3));
    }

    #[test]
    fn shift_levels() {
        assert_eq!(shift_level("** x", true).as_deref(), Some("* x"));
        assert_eq!(shift_level("* x", true), None);
        assert_eq!(shift_level("* x", false).as_deref(), Some("** x"));
        assert_eq!(shift_level("text", false), None);
    }

    #[test]
    fn plan_move_down_and_up() {
        let l = doc("* A\na\n** A1\n* B\nb\n* C");
        // A swaps with B
        assert_eq!(
            plan_move(&l, 0, true),
            Ok(SubtreeMove {
                start: 0,
                mid: 3,
                end: 5
            })
        );
        // C can't go down; B swaps up with A
        assert!(plan_move(&l, 5, true).is_err());
        assert_eq!(
            plan_move(&l, 3, false),
            Ok(SubtreeMove {
                start: 0,
                mid: 3,
                end: 5
            })
        );
        assert!(plan_move(&l, 0, false).is_err());
    }

    #[test]
    fn move_up_stops_at_shallower_heading() {
        let l = doc("* A\n** A1\n* B\n** B1");
        // B1 has no earlier sibling inside B (A1 is under a different parent).
        assert!(plan_move(&l, 3, false).is_err());
    }

    #[test]
    fn todo_cycles_forward_and_back() {
        assert_eq!(cycle_todo("* Task", true).as_deref(), Some("* TODO Task"));
        assert_eq!(
            cycle_todo("* TODO Task", true).as_deref(),
            Some("* DONE Task")
        );
        assert_eq!(cycle_todo("* DONE Task", true).as_deref(), Some("* Task"));
        assert_eq!(cycle_todo("* Task", false).as_deref(), Some("* DONE Task"));
        assert_eq!(cycle_todo("** NEXT x", true).as_deref(), Some("** DONE x"));
        assert_eq!(cycle_todo("not heading", true), None);
    }

    #[test]
    fn todo_on_empty_title() {
        assert_eq!(cycle_todo("* TODO", true).as_deref(), Some("* DONE"));
        assert_eq!(cycle_todo("* DONE", true).as_deref(), Some("* "));
    }

    #[test]
    fn list_continuation_variants() {
        let c = list_continuation("- item").unwrap();
        assert_eq!(c.next_prefix, "- ");
        assert_eq!(c.own_len, 2);
        assert!(!c.empty);

        let c = list_continuation("  3. third").unwrap();
        assert_eq!(c.next_prefix, "  4. ");
        assert_eq!(c.own_len, 5);

        let c = list_continuation("- [X] done").unwrap();
        assert_eq!(c.next_prefix, "- [ ] ");
        assert_eq!(c.own_len, 6);

        let c = list_continuation("- [ ] ").unwrap();
        assert!(c.empty);
        let c = list_continuation("- ").unwrap();
        assert!(c.empty);
    }

    #[test]
    fn non_list_lines() {
        assert!(list_continuation("plain").is_none());
        assert!(list_continuation("-nospace").is_none());
        assert!(list_continuation("1.x").is_none());
        assert!(list_continuation("* heading").is_none());
    }
}
