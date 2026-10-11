//! Scanner for org `[[url]]` / `[[url][description]]` links.
//!
//! Generic over the element type so callers can scan `&[char]` (rendering) or
//! `&[u8]` (cursor logic) with the same state machine.

use std::ops::Range;

/// One link found in a line. All indices are into the scanned slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    /// Index of the first `[` of the opening `[[`.
    pub start: usize,
    pub url: Range<usize>,
    /// `Some` for the `[[url][desc]]` form.
    pub desc: Option<Range<usize>>,
    /// Index of the final `]`.
    pub end: usize,
}

pub fn find_links<T: Copy + PartialEq>(s: &[T], open: T, close: T) -> Vec<LinkSpan> {
    let n = s.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < n {
        if s[i] != open || s[i + 1] != open {
            i += 1;
            continue;
        }
        let start = i;
        i += 2;
        let url_start = i;
        while i < n && s[i] != close {
            i += 1;
        }
        if i >= n {
            break;
        }
        let url = url_start..i;
        i += 1;
        if i >= n {
            break;
        }
        if s[i] == close {
            out.push(LinkSpan {
                start,
                url,
                desc: None,
                end: i,
            });
            i += 1;
        } else if s[i] == open {
            i += 1;
            let desc_start = i;
            while i < n && s[i] != close {
                i += 1;
            }
            if i >= n {
                break;
            }
            let desc = desc_start..i;
            i += 1;
            if i >= n || s[i] != close {
                continue;
            }
            out.push(LinkSpan {
                start,
                url,
                desc: Some(desc),
                end: i,
            });
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(s: &str) -> Vec<LinkSpan> {
        find_links(s.as_bytes(), b'[', b']')
    }

    #[test]
    fn bare_link() {
        assert_eq!(
            links("a [[x]] b"),
            vec![LinkSpan {
                start: 2,
                url: 4..5,
                desc: None,
                end: 6
            }]
        );
    }

    #[test]
    fn described_link() {
        let l = links("[[u][d]]");
        assert_eq!(l[0].url, 2..3);
        assert_eq!(l[0].desc, Some(5..6));
        assert_eq!(l[0].end, 7);
    }

    #[test]
    fn unterminated_is_ignored() {
        assert!(links("[[oops").is_empty());
        assert!(links("[[u][d]").is_empty());
    }

    #[test]
    fn multiple_links() {
        assert_eq!(links("[[a]] [[b][c]]").len(), 2);
    }
}
