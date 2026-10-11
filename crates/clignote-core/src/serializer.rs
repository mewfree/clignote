use std::fmt::Write;

use crate::document::*;

/// Serialize a `Document` back to org-mode text.
///
/// The goal is faithful round-tripping: `serialize(&parse(s))` should equal
/// `s` for well-formed org files. Some edge cases (e.g. exotic whitespace in
/// keyword lines) may not be bit-identical yet.
pub fn serialize(doc: &Document) -> String {
    let mut out = String::new();

    for kw in &doc.keywords {
        // Writing to a `String` is infallible.
        let _ = writeln!(out, "#+{}: {}", kw.key, kw.value);
    }

    for section in &doc.sections {
        serialize_section(&mut out, section);
    }

    out
}

fn serialize_section(out: &mut String, section: &Section) {
    if let Some(h) = &section.headline {
        out.extend(std::iter::repeat_n('*', h.level as usize));
        out.push(' ');
        if let Some(kw) = &h.todo_keyword {
            let _ = write!(out, "{kw} ");
        }
        if let Some(p) = h.priority {
            let _ = write!(out, "[#{p}] ");
        }
        write_inlines(out, &h.title);
        if !h.tags.is_empty() {
            let _ = write!(out, "  :{}:", h.tags.join(":"));
        }
        out.push('\n');
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
                write_inlines(out, line);
                out.push('\n');
            }
        }
        Block::BlankLine => out.push('\n'),
        Block::HorizontalRule => out.push_str("-----\n"),
        Block::SrcBlock(src) => {
            out.push_str("#+begin_src");
            if let Some(lang) = src.language.as_deref().filter(|l| !l.is_empty()) {
                out.push(' ');
                out.push_str(lang);
            }
            out.push('\n');
            if !src.body.is_empty() {
                out.push_str(&src.body);
                out.push('\n');
            }
            out.push_str("#+end_src\n");
        }
        Block::PropertyDrawer(props) => {
            out.push_str(":PROPERTIES:\n");
            for p in props {
                let _ = writeln!(out, ":{}: {}", p.key, p.value);
            }
            out.push_str(":END:\n");
        }
        Block::Drawer { name, lines } => {
            let _ = writeln!(out, ":{name}:");
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
                let _ = write!(out, "{} {cb}", item.bullet);
                write_inlines(out, &item.content);
                out.push('\n');
            }
        }
    }
}

pub fn serialize_inlines(inlines: &[Inline]) -> String {
    let mut out = String::new();
    write_inlines(&mut out, inlines);
    out
}

/// Append `inner` to `out`, delimited by `marker` on both sides.
fn write_wrapped(out: &mut String, marker: char, inner: &[Inline]) {
    out.push(marker);
    write_inlines(out, inner);
    out.push(marker);
}

fn write_inlines(out: &mut String, inlines: &[Inline]) {
    for inline in inlines {
        match inline {
            Inline::Text(s) => out.push_str(s),
            Inline::Bold(inner) => write_wrapped(out, '*', inner),
            Inline::Italic(inner) => write_wrapped(out, '/', inner),
            Inline::Underline(inner) => write_wrapped(out, '_', inner),
            Inline::Strikethrough(inner) => write_wrapped(out, '+', inner),
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
                    write_inlines(out, desc);
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
