/// A parsed org-mode document.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Document {
    /// File-level keywords: #+title:, #+author:, etc.
    pub keywords: Vec<Keyword>,
    /// Top-level sections (preamble + headline sections).
    pub sections: Vec<Section>,
}

/// A #+KEY: VALUE line at the file level.
#[derive(Debug, Clone, PartialEq)]
pub struct Keyword {
    pub key: String,
    pub value: String,
}

/// A section: an optional headline plus its content and children.
///
/// `headline = None` for the preamble (content before the first heading).
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub headline: Option<Headline>,
    pub content: Vec<Block>,
    pub children: Vec<Section>,
}

/// A heading line (`* …`, `** …`, …).
#[derive(Debug, Clone, PartialEq)]
pub struct Headline {
    pub level: u8,
    pub todo_keyword: Option<String>,
    pub priority: Option<char>,
    /// Title as inline elements.
    pub title: Vec<Inline>,
    pub tags: Vec<String>,
}

/// A block-level element inside a section.
///
/// `Paragraph` stores lines individually so the serializer can reconstruct
/// the original line structure for a faithful round-trip.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Each inner `Vec<Inline>` is one source line of a paragraph.
    Paragraph(Vec<Vec<Inline>>),
    List(List),
    SrcBlock(SrcBlock),
    PropertyDrawer(Vec<Property>),
    /// A named drawer other than PROPERTIES (e.g. LOGBOOK).
    Drawer {
        name: String,
        lines: Vec<String>,
    },
    HorizontalRule,
    BlankLine,
}

#[derive(Debug, Clone, PartialEq)]
pub struct List {
    pub items: Vec<ListItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    /// The bullet character(s): "-", "+", "1.", etc.
    pub bullet: String,
    pub checkbox: Option<CheckboxState>,
    pub content: Vec<Inline>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CheckboxState {
    Checked,   // [X]
    Unchecked, // [ ]
    Partial,   // [-]
}

#[derive(Debug, Clone, PartialEq)]
pub struct SrcBlock {
    pub language: Option<String>,
    /// Raw body, preserving internal line breaks.
    pub body: String,
}

/// A :KEY: VALUE line inside a PROPERTIES drawer.
#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    pub key: String,
    pub value: String,
}

/// An inline element.
#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    Bold(Vec<Inline>),
    Italic(Vec<Inline>),
    Underline(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    Code(String),
    Verbatim(String),
    Link(Link),
    Timestamp(Timestamp),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub url: String,
    /// `None` for `[[url]]`, `Some(…)` for `[[url][description]]`.
    pub description: Option<Vec<Inline>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Timestamp {
    pub kind: TimestampKind,
    /// Raw content between the delimiters (e.g. `"2026-03-05 Thu"`).
    pub inner: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TimestampKind {
    Active,   // <…>
    Inactive, // […]
}
