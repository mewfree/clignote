use crate::document::*;
use crate::inline::parse_inline;
use crate::lexer::{tokenize_line, LineToken};

/// Parse an org-mode string into a `Document`.
pub fn parse(input: &str) -> Document {
    let lines: Vec<&str> = input.lines().collect();
    let mut p = Parser {
        lines: &lines,
        pos: 0,
    };
    p.parse_document()
}

// ── Parser state ──────────────────────────────────────────────────────────────

struct Parser<'a> {
    lines: &'a [&'a str],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek_line(&self) -> Option<&'a str> {
        self.lines.get(self.pos).copied()
    }

    fn peek_token(&self) -> Option<LineToken> {
        self.peek_line().map(tokenize_line)
    }

    fn advance(&mut self) -> Option<&'a str> {
        let line = self.lines.get(self.pos).copied();
        self.pos += 1;
        line
    }

    // ── Document ──────────────────────────────────────────────────────────────

    fn parse_document(&mut self) -> Document {
        let mut doc = Document::default();
        let mut preamble: Vec<Block> = Vec::new();

        // Consume preamble: file-level keywords + content before the first headline
        loop {
            match self.peek_token() {
                None => break,
                Some(LineToken::Headline { .. }) => break,
                Some(LineToken::Keyword { key, value }) => {
                    self.advance();
                    doc.keywords.push(Keyword { key, value });
                }
                _ => {
                    if let Some(b) = self.parse_block() {
                        preamble.push(b);
                    }
                }
            }
        }

        if !preamble.is_empty() {
            doc.sections.push(Section {
                headline: None,
                content: preamble,
                children: vec![],
            });
        }

        // Consume top-level sections
        while let Some(tok) = self.peek_token() {
            if let LineToken::Headline { level, .. } = tok {
                if let Some(sec) = self.parse_section(level) {
                    doc.sections.push(sec);
                }
            } else {
                self.advance(); // shouldn't normally happen
            }
        }

        doc
    }

    // ── Section ───────────────────────────────────────────────────────────────

    /// Parse a section whose own headline is at `own_level`.
    fn parse_section(&mut self, own_level: u8) -> Option<Section> {
        // Consume the opening headline
        let headline = match self.peek_token()? {
            LineToken::Headline { level, rest } if level == own_level => {
                self.advance();
                Some(parse_headline(own_level, &rest))
            }
            _ => return None,
        };

        let mut content: Vec<Block> = Vec::new();
        let mut children: Vec<Section> = Vec::new();

        loop {
            match self.peek_token() {
                None => break,
                Some(LineToken::Headline { level, .. }) => {
                    if level <= own_level {
                        break; // sibling or ancestor — leave for caller
                    }
                    if let Some(child) = self.parse_section(level) {
                        children.push(child);
                    }
                }
                _ => {
                    if let Some(b) = self.parse_block() {
                        content.push(b);
                    }
                }
            }
        }

        Some(Section {
            headline,
            content,
            children,
        })
    }

    // ── Blocks ────────────────────────────────────────────────────────────────

    fn parse_block(&mut self) -> Option<Block> {
        match self.peek_token()? {
            LineToken::Blank => {
                self.advance();
                Some(Block::BlankLine)
            }
            LineToken::HorizontalRule => {
                self.advance();
                Some(Block::HorizontalRule)
            }
            LineToken::BeginBlock { kind, params } => self.parse_src_or_block(&kind, &params),
            LineToken::DrawerBegin(name) => self.parse_drawer(&name),
            LineToken::ListItem { .. } => self.parse_list(),
            _ => self.parse_paragraph(),
        }
    }

    fn parse_src_or_block(&mut self, kind: &str, params: &str) -> Option<Block> {
        self.advance(); // consume #+begin_…

        if kind == "SRC" {
            let language = params
                .split_whitespace()
                .next()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            let mut body_lines: Vec<&str> = Vec::new();
            loop {
                match self.peek_line() {
                    None => break,
                    Some(line) => match tokenize_line(line) {
                        LineToken::EndBlock(_) => {
                            self.advance();
                            break;
                        }
                        _ => {
                            body_lines.push(line);
                            self.advance();
                        }
                    },
                }
            }
            Some(Block::SrcBlock(SrcBlock {
                language,
                body: body_lines.join("\n"),
            }))
        } else {
            // Generic block: skip until #+end_KIND
            loop {
                match self.peek_token() {
                    None => break,
                    Some(LineToken::EndBlock(k)) if k == kind => {
                        self.advance();
                        break;
                    }
                    _ => {
                        self.advance();
                    }
                }
            }
            Some(Block::BlankLine) // TODO: implement QuoteBlock etc.
        }
    }

    fn parse_drawer(&mut self, name: &str) -> Option<Block> {
        self.advance(); // consume :NAME:
        let is_props = name.eq_ignore_ascii_case("PROPERTIES");
        let mut properties: Vec<Property> = Vec::new();
        let mut raw_lines: Vec<String> = Vec::new();

        loop {
            match self.peek_token() {
                None | Some(LineToken::DrawerEnd) => {
                    self.advance();
                    break;
                }
                Some(LineToken::DrawerProperty { key, value }) => {
                    if is_props {
                        properties.push(Property { key, value });
                    } else {
                        raw_lines.push(format!(":{}: {}", key, value));
                    }
                    self.advance();
                }
                _ => {
                    if let Some(line) = self.advance() {
                        raw_lines.push(line.to_string());
                    }
                }
            }
        }

        if is_props {
            Some(Block::PropertyDrawer(properties))
        } else {
            Some(Block::Drawer {
                name: name.to_string(),
                lines: raw_lines,
            })
        }
    }

    fn parse_list(&mut self) -> Option<Block> {
        let mut items: Vec<ListItem> = Vec::new();

        while let Some(LineToken::ListItem { bullet, rest, .. }) = self.peek_token() {
            self.advance();
            let (checkbox, text) = parse_checkbox(&rest);
            items.push(ListItem {
                bullet,
                checkbox,
                content: parse_inline(&text),
            });
        }

        if items.is_empty() {
            None
        } else {
            Some(Block::List(List { items }))
        }
    }

    fn parse_paragraph(&mut self) -> Option<Block> {
        let mut lines: Vec<Vec<Inline>> = Vec::new();

        loop {
            match self.peek_token() {
                Some(LineToken::Text(s)) => {
                    lines.push(parse_inline(&s));
                    self.advance();
                }
                _ => break,
            }
        }

        if lines.is_empty() {
            None
        } else {
            Some(Block::Paragraph(lines))
        }
    }
}

// ── Headline parsing ──────────────────────────────────────────────────────────

/// Known TODO keywords. Extend as needed.
const TODO_KEYWORDS: &[&str] = &[
    "TODO",
    "DONE",
    "DOING",
    "NEXT",
    "WAITING",
    "HOLD",
    "CANCELLED",
];

fn parse_headline(level: u8, rest: &str) -> Headline {
    let (title_and_tags, tags) = extract_tags(rest.trim());
    let s = title_and_tags.trim();

    // TODO keyword
    let (todo_keyword, s) = extract_todo_keyword(s);
    let s = s.trim_start();

    // Priority [#A]
    let (priority, s) = extract_priority(s);
    let s = s.trim_start();

    Headline {
        level,
        todo_keyword,
        priority,
        title: parse_inline(s),
        tags,
    }
}

fn extract_todo_keyword(s: &str) -> (Option<String>, &str) {
    for &kw in TODO_KEYWORDS {
        if let Some(rest) = s.strip_prefix(kw) {
            if rest.is_empty() || rest.starts_with(' ') {
                return (Some(kw.to_string()), rest);
            }
        }
    }
    (None, s)
}

fn extract_priority(s: &str) -> (Option<char>, &str) {
    if s.starts_with("[#") && s.len() >= 4 {
        let b = s.as_bytes();
        if b[3] == b']' {
            return (Some(b[2] as char), &s[4..]);
        }
    }
    (None, s)
}

/// Separate the trailing tag group (`:tag1:tag2:`) from the title.
///
/// Tags must be at the end of the line, preceded by whitespace, in the form
/// `:word1:word2:…:` where each word contains only tag-valid characters.
fn extract_tags(s: &str) -> (String, Vec<String>) {
    if !s.ends_with(':') {
        return (s.to_string(), vec![]);
    }

    let bytes = s.as_bytes();
    // `pos` always points to the position of a ':' we're about to look left of.
    // We start from the final ':'.
    let mut pos = s.len() - 1; // index of the last ':'
    let mut segments: Vec<&str> = Vec::new();

    loop {
        // Scan backwards from pos-1 to find the start of the current tag segment.
        let seg_end = pos;
        let mut seg_start = pos;
        while seg_start > 0 && bytes[seg_start - 1] != b':' {
            seg_start -= 1;
        }
        let tag = &s[seg_start..seg_end];

        if tag.is_empty() || !tag.chars().all(is_tag_char) {
            break;
        }
        segments.push(tag);

        if seg_start == 0 {
            break; // no opening ':'
        }
        // bytes[seg_start - 1] == b':'  (the ':' that opens this segment)
        let opening_colon = seg_start - 1;

        if opening_colon == 0 {
            // Opening ':' is at the very start of the string — no space before it.
            break;
        }
        let before_opening = bytes[opening_colon - 1];
        if before_opening.is_ascii_whitespace() {
            // Found the full tag section.
            let tag_str = &s[opening_colon..]; // ":tag1:tag2:…:"
            let tags: Vec<String> = tag_str[1..tag_str.len() - 1]
                .split(':')
                .filter(|t| !t.is_empty() && t.chars().all(is_tag_char))
                .map(|t| t.to_string())
                .collect();
            if !tags.is_empty() {
                return (s[..opening_colon].trim_end().to_string(), tags);
            }
            break;
        }
        if before_opening == b':' {
            // There's another colon — means we're inside a longer tag section;
            // this shouldn't happen with the way we walk, but be safe.
            break;
        }
        // The char before the opening ':' is a tag char (i.e. ':' is mid-word).
        // Not a valid tag section boundary — keep walking left to find the real one.
        pos = opening_colon;
    }

    (s.to_string(), vec![])
}

fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '@' || c == '#' || c == '%'
}

// ── List checkbox ─────────────────────────────────────────────────────────────

fn parse_checkbox(rest: &str) -> (Option<CheckboxState>, String) {
    if let Some(s) = rest
        .strip_prefix("[X] ")
        .or_else(|| rest.strip_prefix("[x] "))
    {
        return (Some(CheckboxState::Checked), s.to_string());
    }
    if let Some(s) = rest.strip_prefix("[ ] ") {
        return (Some(CheckboxState::Unchecked), s.to_string());
    }
    if let Some(s) = rest.strip_prefix("[-] ") {
        return (Some(CheckboxState::Partial), s.to_string());
    }
    (None, rest.to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Inline {
        Inline::Text(s.to_string())
    }

    #[test]
    fn parse_empty() {
        let doc = parse("");
        assert!(doc.keywords.is_empty());
        assert!(doc.sections.is_empty());
    }

    #[test]
    fn parse_keywords() {
        let doc = parse("#+title: My File\n#+author: Damien\n");
        assert_eq!(
            doc.keywords[0],
            Keyword {
                key: "title".into(),
                value: "My File".into()
            }
        );
        assert_eq!(
            doc.keywords[1],
            Keyword {
                key: "author".into(),
                value: "Damien".into()
            }
        );
    }

    #[test]
    fn parse_single_heading() {
        let doc = parse("* Hello\n");
        assert_eq!(doc.sections.len(), 1);
        let h = doc.sections[0].headline.as_ref().unwrap();
        assert_eq!(h.level, 1);
        assert_eq!(h.title, vec![text("Hello")]);
    }

    #[test]
    fn parse_todo_heading() {
        let doc = parse("* TODO Buy groceries\n");
        let h = doc.sections[0].headline.as_ref().unwrap();
        assert_eq!(h.todo_keyword, Some("TODO".into()));
        assert_eq!(h.title, vec![text("Buy groceries")]);
    }

    #[test]
    fn parse_heading_with_tags() {
        let doc = parse("* My heading :work:urgent:\n");
        let h = doc.sections[0].headline.as_ref().unwrap();
        assert_eq!(h.tags, vec!["work", "urgent"]);
        assert_eq!(h.title, vec![text("My heading")]);
    }

    #[test]
    fn parse_nested_sections() {
        let src = "* Parent\n** Child\n*** Grandchild\n";
        let doc = parse(src);
        assert_eq!(doc.sections.len(), 1);
        assert_eq!(doc.sections[0].children.len(), 1);
        assert_eq!(doc.sections[0].children[0].children.len(), 1);
    }

    #[test]
    fn parse_paragraph() {
        let doc = parse("* H\nLine one\nLine two\n");
        let content = &doc.sections[0].content;
        assert!(matches!(content[0], Block::Paragraph(_)));
    }

    #[test]
    fn parse_src_block() {
        let src = "#+begin_src rust\nfn main() {}\n#+end_src\n";
        let doc = parse(src);
        let preamble = &doc.sections[0].content;
        assert!(matches!(preamble[0], Block::SrcBlock(_)));
        if let Block::SrcBlock(sb) = &preamble[0] {
            assert_eq!(sb.language, Some("rust".into()));
            assert_eq!(sb.body, "fn main() {}");
        }
    }

    #[test]
    fn parse_properties_drawer() {
        let src = "* H\n:PROPERTIES:\n:ID: abc123\n:END:\n";
        let doc = parse(src);
        let content = &doc.sections[0].content;
        assert!(matches!(content[0], Block::PropertyDrawer(_)));
        if let Block::PropertyDrawer(props) = &content[0] {
            assert_eq!(props[0].key, "ID");
            assert_eq!(props[0].value, "abc123");
        }
    }

    #[test]
    fn parse_unordered_list() {
        let src = "- alpha\n- beta\n- gamma\n";
        let doc = parse(src);
        let content = &doc.sections[0].content;
        assert!(matches!(content[0], Block::List(_)));
        if let Block::List(list) = &content[0] {
            assert_eq!(list.items.len(), 3);
        }
    }
}
