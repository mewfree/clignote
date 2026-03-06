use crate::document::*;

/// Serialize a `Document` back to org-mode text.
///
/// The goal is faithful round-tripping: `serialize(&parse(s))` should equal
/// `s` for well-formed org files. Some edge cases (e.g. exotic whitespace in
/// keyword lines) may not be bit-identical yet.
pub fn serialize(doc: &Document) -> String {
    let mut out = String::new();

    for kw in &doc.keywords {
        out.push_str(&format!("#+{}: {}\n", kw.key, kw.value));
    }

    for section in &doc.sections {
        serialize_section(&mut out, section);
    }

    out
}

fn serialize_section(out: &mut String, section: &Section) {
    if let Some(h) = &section.headline {
        let stars = "*".repeat(h.level as usize);
        let kw = h.todo_keyword.as_deref().map(|k| format!("{} ", k)).unwrap_or_default();
        let pri = h.priority.map(|p| format!("[#{}] ", p)).unwrap_or_default();
        let title = serialize_inlines(&h.title);
        let tags = if h.tags.is_empty() {
            String::new()
        } else {
            format!("  :{}:", h.tags.join(":"))
        };
        out.push_str(&format!("{} {}{}{}{}\n", stars, kw, pri, title, tags));
    }

    for block in &section.content {
        serialize_block(out, block);
    }

    for child in &section.children {
        serialize_section(out, child);
    }
}

fn serialize_block(out: &mut String, block: &Block) {
    match block {
        Block::Paragraph(lines) => {
            for line in lines {
                out.push_str(&serialize_inlines(line));
                out.push('\n');
            }
        }
        Block::BlankLine => out.push('\n'),
        Block::HorizontalRule => out.push_str("-----\n"),
        Block::SrcBlock(src) => {
            let lang = src.language.as_deref().unwrap_or("");
            if lang.is_empty() {
                out.push_str("#+begin_src\n");
            } else {
                out.push_str(&format!("#+begin_src {}\n", lang));
            }
            if !src.body.is_empty() {
                out.push_str(&src.body);
                out.push('\n');
            }
            out.push_str("#+end_src\n");
        }
        Block::PropertyDrawer(props) => {
            out.push_str(":PROPERTIES:\n");
            for p in props {
                out.push_str(&format!(":{}: {}\n", p.key, p.value));
            }
            out.push_str(":END:\n");
        }
        Block::Drawer { name, lines } => {
            out.push_str(&format!(":{}:\n", name));
            for line in lines {
                out.push_str(line);
                out.push('\n');
            }
            out.push_str(":END:\n");
        }
        Block::List(list) => {
            for item in &list.items {
                let cb = match item.checkbox {
                    Some(CheckboxState::Checked) => "[X] ",
                    Some(CheckboxState::Unchecked) => "[ ] ",
                    Some(CheckboxState::Partial) => "[-] ",
                    None => "",
                };
                out.push_str(&format!("{} {}{}\n", item.bullet, cb, serialize_inlines(&item.content)));
            }
        }
    }
}

pub fn serialize_inlines(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(s) => out.push_str(s),
            Inline::Bold(inner) => {
                out.push('*');
                out.push_str(&serialize_inlines(inner));
                out.push('*');
            }
            Inline::Italic(inner) => {
                out.push('/');
                out.push_str(&serialize_inlines(inner));
                out.push('/');
            }
            Inline::Underline(inner) => {
                out.push('_');
                out.push_str(&serialize_inlines(inner));
                out.push('_');
            }
            Inline::Strikethrough(inner) => {
                out.push('+');
                out.push_str(&serialize_inlines(inner));
                out.push('+');
            }
            Inline::Code(s) => {
                out.push('~');
                out.push_str(s);
                out.push('~');
            }
            Inline::Verbatim(s) => {
                out.push('=');
                out.push_str(s);
                out.push('=');
            }
            Inline::Link(link) => {
                out.push_str("[[");
                out.push_str(&link.url);
                out.push(']');
                if let Some(desc) = &link.description {
                    out.push('[');
                    out.push_str(&serialize_inlines(desc));
                    out.push(']');
                }
                out.push(']');
            }
            Inline::Timestamp(ts) => {
                let (open, close) = match ts.kind {
                    TimestampKind::Active => ('<', '>'),
                    TimestampKind::Inactive => ('[', ']'),
                };
                out.push(open);
                out.push_str(&ts.inner);
                out.push(close);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn roundtrip(src: &str) -> String {
        serialize(&parse(src))
    }

    #[test]
    fn keyword_roundtrip() {
        let src = "#+title: Hello\n#+author: Damien\n";
        assert_eq!(roundtrip(src), src);
    }

    #[test]
    fn heading_roundtrip() {
        let src = "* Hello\n";
        assert_eq!(roundtrip(src), src);
    }

    #[test]
    fn todo_heading_roundtrip() {
        let src = "* TODO Buy groceries\n";
        assert_eq!(roundtrip(src), src);
    }

    #[test]
    fn nested_headings_roundtrip() {
        let src = "* Parent\n** Child\n";
        assert_eq!(roundtrip(src), src);
    }

    #[test]
    fn src_block_roundtrip() {
        let src = "#+begin_src rust\nfn main() {}\n#+end_src\n";
        assert_eq!(roundtrip(src), src);
    }

    #[test]
    fn list_roundtrip() {
        let src = "- alpha\n- beta\n";
        assert_eq!(roundtrip(src), src);
    }

    #[test]
    fn properties_roundtrip() {
        let src = "* H\n:PROPERTIES:\n:ID: abc\n:END:\n";
        assert_eq!(roundtrip(src), src);
    }
}
