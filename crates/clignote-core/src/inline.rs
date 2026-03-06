use crate::document::{Inline, Link, Timestamp, TimestampKind};

/// Parse an org-mode inline string into a sequence of `Inline` elements.
pub fn parse_inline(text: &str) -> Vec<Inline> {
    let chars: Vec<char> = text.chars().collect();
    parse_span(&chars, 0, chars.len())
}

// ── Internal ──────────────────────────────────────────────────────────────────

fn parse_span(chars: &[char], start: usize, end: usize) -> Vec<Inline> {
    let mut result: Vec<Inline> = Vec::new();
    let mut i = start;
    let mut text_buf = String::new();

    while i < end {
        let c = chars[i];

        // [[link]] or [[link][desc]]
        if c == '[' && i + 1 < end && chars[i + 1] == '[' {
            if let Some((link, next)) = try_link(chars, i) {
                flush_text(&mut text_buf, &mut result);
                result.push(Inline::Link(link));
                i = next;
                continue;
            }
        }

        // Inline markup: * / _ + = ~
        if matches!(c, '*' | '/' | '_' | '+' | '=' | '~') {
            let pre_ok = i == start || !chars[i - 1].is_alphanumeric();
            if pre_ok {
                if let Some((inline, next)) = try_markup(chars, i, end) {
                    flush_text(&mut text_buf, &mut result);
                    result.push(inline);
                    i = next;
                    continue;
                }
            }
        }

        // Active timestamp <…>
        if c == '<' {
            if let Some((ts, next)) = try_timestamp(chars, i, TimestampKind::Active, '<', '>') {
                flush_text(&mut text_buf, &mut result);
                result.push(Inline::Timestamp(ts));
                i = next;
                continue;
            }
        }

        // Inactive timestamp [YYYY-…] (not [[link]])
        if c == '[' && i + 1 < end && chars[i + 1] != '[' {
            if let Some((ts, next)) = try_inactive_timestamp(chars, i) {
                flush_text(&mut text_buf, &mut result);
                result.push(Inline::Timestamp(ts));
                i = next;
                continue;
            }
        }

        text_buf.push(c);
        i += 1;
    }

    flush_text(&mut text_buf, &mut result);
    result
}

fn flush_text(buf: &mut String, out: &mut Vec<Inline>) {
    if !buf.is_empty() {
        out.push(Inline::Text(std::mem::take(buf)));
    }
}

/// Try to parse an inline markup span starting at `start`.
/// Returns `(Inline, next_pos)` or `None` if not valid markup.
fn try_markup(chars: &[char], start: usize, end: usize) -> Option<(Inline, usize)> {
    let marker = chars[start];
    let content_start = start + 1;
    if content_start >= end {
        return None;
    }
    // Must not open with whitespace
    if chars[content_start].is_whitespace() {
        return None;
    }

    let mut i = content_start + 1;
    while i < end {
        if chars[i] == marker && !chars[i - 1].is_whitespace() {
            // Must not close before an alphanumeric
            let post_ok = i + 1 >= end || !chars[i + 1].is_alphanumeric();
            if post_ok {
                let inline = build_markup(marker, chars, content_start, i);
                return Some((inline, i + 1));
            }
        }
        i += 1;
    }
    None
}

fn build_markup(marker: char, chars: &[char], from: usize, to: usize) -> Inline {
    let content: String = chars[from..to].iter().collect();
    match marker {
        '*' => Inline::Bold(parse_span(chars, from, to)),
        '/' => Inline::Italic(parse_span(chars, from, to)),
        '_' => Inline::Underline(parse_span(chars, from, to)),
        '+' => Inline::Strikethrough(parse_span(chars, from, to)),
        '=' => Inline::Verbatim(content),
        '~' => Inline::Code(content),
        _ => unreachable!(),
    }
}

/// Try to parse `[[url]]` or `[[url][desc]]` at position `start`.
fn try_link(chars: &[char], start: usize) -> Option<(Link, usize)> {
    debug_assert!(chars[start] == '[' && chars.get(start + 1) == Some(&'['));
    let mut i = start + 2;

    // Collect URL until ']'
    let url_start = i;
    while i < chars.len() && chars[i] != ']' {
        i += 1;
    }
    if i >= chars.len() {
        return None;
    }
    let url: String = chars[url_start..i].iter().collect();
    i += 1; // skip ']'

    if i >= chars.len() {
        return None;
    }
    match chars[i] {
        ']' => Some((Link { url, description: None }, i + 1)),
        '[' => {
            i += 1;
            let desc_start = i;
            while i < chars.len() && chars[i] != ']' {
                i += 1;
            }
            if i >= chars.len() {
                return None;
            }
            let desc = parse_span(chars, desc_start, i);
            i += 1; // skip ']'
            if i >= chars.len() || chars[i] != ']' {
                return None;
            }
            Some((Link { url, description: Some(desc) }, i + 1))
        }
        _ => None,
    }
}

/// Try to parse an active `<…>` or inactive `[…]` timestamp.
fn try_timestamp(
    chars: &[char],
    start: usize,
    kind: TimestampKind,
    open: char,
    close: char,
) -> Option<(Timestamp, usize)> {
    debug_assert!(chars[start] == open);
    let mut i = start + 1;
    let inner_start = i;
    while i < chars.len() && chars[i] != close && chars[i] != '\n' {
        i += 1;
    }
    if i >= chars.len() || chars[i] != close {
        return None;
    }
    let inner: String = chars[inner_start..i].iter().collect();
    Some((Timestamp { kind, inner }, i + 1))
}

/// Try to parse an inactive timestamp `[YYYY-MM-DD …]` at position `start`.
/// Requires the content to start with four digits and a dash.
fn try_inactive_timestamp(chars: &[char], start: usize) -> Option<(Timestamp, usize)> {
    debug_assert!(chars[start] == '[');
    // Quick filter: must begin with digit
    if start + 1 >= chars.len() || !chars[start + 1].is_ascii_digit() {
        return None;
    }
    let mut i = start + 1;
    let inner_start = i;
    while i < chars.len() && chars[i] != ']' && chars[i] != '\n' {
        i += 1;
    }
    if i >= chars.len() || chars[i] != ']' {
        return None;
    }
    let inner: String = chars[inner_start..i].iter().collect();
    // Validate rough date pattern: NNNN-NN-NN
    let b = inner.as_bytes();
    if b.len() >= 10
        && b[..4].iter().all(|c| c.is_ascii_digit())
        && b[4] == b'-'
        && b[5..7].iter().all(|c| c.is_ascii_digit())
        && b[7] == b'-'
        && b[8..10].iter().all(|c| c.is_ascii_digit())
    {
        Some((Timestamp { kind: TimestampKind::Inactive, inner }, i + 1))
    } else {
        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text() {
        assert_eq!(parse_inline("hello"), vec![Inline::Text("hello".into())]);
    }

    #[test]
    fn bold() {
        assert_eq!(
            parse_inline("*bold*"),
            vec![Inline::Bold(vec![Inline::Text("bold".into())])]
        );
    }

    #[test]
    fn italic() {
        assert_eq!(
            parse_inline("/italic/"),
            vec![Inline::Italic(vec![Inline::Text("italic".into())])]
        );
    }

    #[test]
    fn code() {
        assert_eq!(parse_inline("~code~"), vec![Inline::Code("code".into())]);
    }

    #[test]
    fn verbatim() {
        assert_eq!(parse_inline("=verb="), vec![Inline::Verbatim("verb".into())]);
    }

    #[test]
    fn link_bare() {
        assert_eq!(
            parse_inline("[[https://example.com]]"),
            vec![Inline::Link(Link { url: "https://example.com".into(), description: None })]
        );
    }

    #[test]
    fn link_with_desc() {
        assert_eq!(
            parse_inline("[[https://example.com][Example]]"),
            vec![Inline::Link(Link {
                url: "https://example.com".into(),
                description: Some(vec![Inline::Text("Example".into())]),
            })]
        );
    }

    #[test]
    fn mixed() {
        let result = parse_inline("Hello *world* and ~code~");
        assert_eq!(
            result,
            vec![
                Inline::Text("Hello ".into()),
                Inline::Bold(vec![Inline::Text("world".into())]),
                Inline::Text(" and ".into()),
                Inline::Code("code".into()),
            ]
        );
    }

    #[test]
    fn no_markup_in_word() {
        // Asterisk inside a word should not trigger markup
        let result = parse_inline("he*llo*world");
        // 'he' precedes '*', so pre_ok fails — treated as text
        assert_eq!(result, vec![Inline::Text("he*llo*world".into())]);
    }

    #[test]
    fn active_timestamp() {
        let result = parse_inline("<2026-03-05 Thu>");
        assert_eq!(
            result,
            vec![Inline::Timestamp(Timestamp {
                kind: TimestampKind::Active,
                inner: "2026-03-05 Thu".into(),
            })]
        );
    }
}
