//! Soft-wrap layout helpers.
//!
//! Wrapping works on *display* characters: the buffer line with `[[url][desc]]`
//! links concealed when link concealing is on. Both the renderer and the
//! pane's movement/scroll logic go through these functions so they always
//! agree on how many screen rows a line takes.

use std::ops::Range;

use crate::links;

/// How a pane lays out long lines. Refreshed by the renderer every frame so
/// input handling sees the geometry of the last draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WrapState {
    /// Content width in columns (gutter excluded).
    pub width: usize,
    pub conceal_links: bool,
    /// The pane is active and in Insert mode: the cursor row shows raw link
    /// syntax and may have its cursor sit one cell past the end.
    pub insert: bool,
}

/// Which chars survive concealing links, or `None` if the line has no links.
/// Dropped: the `[[`, the URL and `][` of a described link, and the closing `]]`.
pub fn conceal_mask(chars: &[char]) -> Option<Vec<bool>> {
    let found = links::find_links(chars, '[', ']');
    if found.is_empty() {
        return None;
    }
    let mut keep = vec![true; chars.len()];
    for link in &found {
        let text = link.desc.clone().unwrap_or_else(|| link.url.clone());
        keep[link.start..text.start].fill(false);
        keep[text.end..=link.end].fill(false);
    }
    Some(keep)
}

/// The characters shown for `line`, with links concealed if requested.
pub fn display_chars(line: &str, conceal: bool) -> Vec<char> {
    let chars: Vec<char> = line.chars().collect();
    if !conceal {
        return chars;
    }
    match conceal_mask(&chars) {
        None => chars,
        Some(keep) => chars
            .into_iter()
            .zip(keep)
            .filter_map(|(c, k)| k.then_some(c))
            .collect(),
    }
}

/// A single segment covering `n` cells.
#[allow(clippy::single_range_in_vec_init)]
pub fn whole(n: usize) -> Vec<Range<usize>> {
    vec![0..n]
}

/// Split `chars` into screen rows of at most `width` cells, breaking after the
/// last space that fits and hard-breaking words longer than a row. Always
/// returns at least one (possibly empty) range.
pub fn segments(chars: &[char], width: usize) -> Vec<Range<usize>> {
    let n = chars.len();
    if width == 0 || n <= width {
        return whole(n);
    }
    let mut out = Vec::new();
    let mut pos = 0;
    while n - pos > width {
        let end = chars[pos..pos + width]
            .iter()
            .rposition(|&c| c == ' ')
            .filter(|&i| i > 0)
            .map_or(pos + width, |i| pos + i + 1);
        out.push(pos..end);
        pos = end;
    }
    out.push(pos..n);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(s: &str, w: usize) -> Vec<String> {
        let chars: Vec<char> = s.chars().collect();
        segments(&chars, w)
            .into_iter()
            .map(|r| chars[r].iter().collect())
            .collect()
    }

    #[test]
    fn short_lines_are_one_segment() {
        assert_eq!(seg("hello", 10), vec!["hello"]);
        assert_eq!(seg("", 10), vec![""]);
        assert_eq!(seg("exactly10!", 10), vec!["exactly10!"]);
    }

    #[test]
    fn breaks_after_spaces() {
        assert_eq!(
            seg("the quick brown fox", 10),
            vec!["the quick ", "brown fox"]
        );
    }

    #[test]
    fn hard_breaks_long_words() {
        assert_eq!(seg("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn segments_cover_the_line_exactly() {
        let s = "a bb ccc dddd eeeee ffffff ggggggg";
        for w in 1..20 {
            assert_eq!(seg(s, w).concat(), s);
        }
    }

    #[test]
    fn conceal_drops_link_syntax() {
        let shown: String = display_chars("a [[u][desc]] b [[x]]", true)
            .into_iter()
            .collect();
        assert_eq!(shown, "a desc b x");
        let raw: String = display_chars("a [[u][desc]]", false).into_iter().collect();
        assert_eq!(raw, "a [[u][desc]]");
    }
}
