/// A line-level token. The parser processes these to build the document tree.
#[derive(Debug, Clone, PartialEq)]
pub enum LineToken {
    /// `* Title`, `** Title`, …
    Headline {
        level: u8,
        rest: String,
    },
    /// `#+KEY: VALUE` (not a block keyword)
    Keyword {
        key: String,
        value: String,
    },
    /// `#+begin_KIND params`
    BeginBlock {
        kind: String,
        params: String,
    },
    /// `#+end_KIND`
    EndBlock(String),
    /// `:NAME:` on its own line (drawer open)
    DrawerBegin(String),
    /// `:END:`
    DrawerEnd,
    /// `:KEY: VALUE` (property inside a drawer)
    DrawerProperty {
        key: String,
        value: String,
    },
    /// `- text`, `+ text`, `1. text`, …
    ListItem {
        indent: usize,
        bullet: String,
        rest: String,
    },
    HorizontalRule,
    Blank,
    /// Any other text line.
    Text(String),
}

/// Classify a single source line.
pub fn tokenize_line(line: &str) -> LineToken {
    if line.trim().is_empty() {
        return LineToken::Blank;
    }

    // ── Headings ─────────────────────────────────────────────────────────────
    if line.starts_with('*') {
        let level = line.chars().take_while(|&c| c == '*').count();
        let after = &line[level..];
        if after.starts_with(' ') {
            return LineToken::Headline {
                level: level as u8,
                rest: after[1..].to_string(),
            };
        }
    }

    // ── #+… lines ────────────────────────────────────────────────────────────
    if let Some(rest) = line.strip_prefix("#+") {
        // #+begin_KIND [PARAMS]  — no colon; split by first whitespace
        if rest.to_lowercase().starts_with("begin_") {
            let after = &rest["begin_".len()..]; // e.g. "src rust"
            let kind_len = after
                .find(|c: char| c.is_ascii_whitespace())
                .unwrap_or(after.len());
            let kind = after[..kind_len].to_uppercase();
            let params = after[kind_len..].trim().to_string();
            return LineToken::BeginBlock { kind, params };
        }

        // #+end_KIND
        if rest.to_lowercase().starts_with("end_") {
            let after = &rest["end_".len()..];
            let kind_len = after
                .find(|c: char| c.is_ascii_whitespace() || c == ':')
                .unwrap_or(after.len());
            let kind = after[..kind_len].to_uppercase();
            return LineToken::EndBlock(kind);
        }

        // #+KEY: VALUE
        let colon = rest.find(':').unwrap_or(rest.len());
        let key = &rest[..colon];
        let value = if colon < rest.len() {
            rest[colon + 1..].trim()
        } else {
            ""
        };
        return LineToken::Keyword {
            key: key.to_string(),
            value: value.to_string(),
        };
    }

    // ── Drawer / property lines ───────────────────────────────────────────────
    if line.starts_with(':') {
        let trimmed = line.trim_end();
        if trimmed.eq_ignore_ascii_case(":end:") {
            return LineToken::DrawerEnd;
        }
        // Look for second ':'
        if let Some(close) = trimmed[1..].find(':') {
            let name = &trimmed[1..1 + close];
            let after = trimmed[1 + close + 1..].trim();
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            {
                if after.is_empty() {
                    return LineToken::DrawerBegin(name.to_string());
                } else {
                    return LineToken::DrawerProperty {
                        key: name.to_string(),
                        value: after.to_string(),
                    };
                }
            }
        }
    }

    // ── Horizontal rule: 5+ dashes ────────────────────────────────────────────
    {
        let t = line.trim();
        if t.len() >= 5 && t.chars().all(|c| c == '-') {
            return LineToken::HorizontalRule;
        }
    }

    // ── List items ────────────────────────────────────────────────────────────
    let indent = line.len() - line.trim_start().len();
    let tail = &line[indent..];

    if let Some(rest) = tail.strip_prefix("- ").or_else(|| tail.strip_prefix("+ ")) {
        let bullet = if tail.starts_with("- ") { "-" } else { "+" };
        return LineToken::ListItem {
            indent,
            bullet: bullet.to_string(),
            rest: rest.to_string(),
        };
    }
    // Ordered list: "1. ", "2. ", …
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let after_digits = &tail[digits.len()..];
        if let Some(rest) = after_digits.strip_prefix(". ") {
            return LineToken::ListItem {
                indent,
                bullet: format!("{}.", digits),
                rest: rest.to_string(),
            };
        }
    }

    LineToken::Text(line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_level_1() {
        assert_eq!(
            tokenize_line("* Hello"),
            LineToken::Headline {
                level: 1,
                rest: "Hello".into()
            }
        );
    }

    #[test]
    fn heading_level_3() {
        assert_eq!(
            tokenize_line("*** Deep"),
            LineToken::Headline {
                level: 3,
                rest: "Deep".into()
            }
        );
    }

    #[test]
    fn keyword_line() {
        assert_eq!(
            tokenize_line("#+title: My Doc"),
            LineToken::Keyword {
                key: "title".into(),
                value: "My Doc".into()
            }
        );
    }

    #[test]
    fn begin_src() {
        assert_eq!(
            tokenize_line("#+begin_src rust"),
            LineToken::BeginBlock {
                kind: "SRC".into(),
                params: "rust".into()
            }
        );
    }

    #[test]
    fn end_src() {
        assert_eq!(
            tokenize_line("#+end_src"),
            LineToken::EndBlock("SRC".into())
        );
    }

    #[test]
    fn drawer_begin() {
        assert_eq!(
            tokenize_line(":PROPERTIES:"),
            LineToken::DrawerBegin("PROPERTIES".into())
        );
    }

    #[test]
    fn drawer_end() {
        assert_eq!(tokenize_line(":END:"), LineToken::DrawerEnd);
    }

    #[test]
    fn property_line() {
        assert_eq!(
            tokenize_line(":CREATED: 2026-03-05"),
            LineToken::DrawerProperty {
                key: "CREATED".into(),
                value: "2026-03-05".into()
            }
        );
    }

    #[test]
    fn list_unordered() {
        assert_eq!(
            tokenize_line("- item"),
            LineToken::ListItem {
                indent: 0,
                bullet: "-".into(),
                rest: "item".into()
            }
        );
    }

    #[test]
    fn list_ordered() {
        assert_eq!(
            tokenize_line("1. first"),
            LineToken::ListItem {
                indent: 0,
                bullet: "1.".into(),
                rest: "first".into()
            }
        );
    }

    #[test]
    fn horizontal_rule() {
        assert_eq!(tokenize_line("-----"), LineToken::HorizontalRule);
    }

    #[test]
    fn blank_line() {
        assert_eq!(tokenize_line(""), LineToken::Blank);
        assert_eq!(tokenize_line("   "), LineToken::Blank);
    }
}
