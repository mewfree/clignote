use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

// ── Action ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneDir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    GoToFileStart,
    GoToFileEnd,
    DeleteLine,
    DeleteWord,
    DeleteChar,
    YankLine,
    SplitHorizontal,
    SplitVertical,
    ClosePane,
    NextPane,
    FocusPane(PaneDir),
    SaveFile,
    QuitAll,
}

// ── Sequence matching ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchResult {
    Action(Action),
    /// Current sequence is a known prefix — wait for more keys.
    Prefix,
    /// No binding starts with this sequence.
    NoMatch,
}

/// All recognised multi-key normal-mode sequences (string form → action).
///
/// Single-key bindings (h, j, k, l, i, …) are handled separately in app.rs.
fn exact_match(s: &str) -> Option<Action> {
    match s {
        "g g" => Some(Action::GoToFileStart),
        "d d" => Some(Action::DeleteLine),
        "d w" => Some(Action::DeleteWord),
        "y y" => Some(Action::YankLine),
        // Vim C-w window commands
        "C-w s" | "C-w C-s" => Some(Action::SplitHorizontal),
        "C-w v" | "C-w C-v" => Some(Action::SplitVertical),
        "C-w w" | "C-w C-w" => Some(Action::NextPane),
        "C-w c" | "C-w q" => Some(Action::ClosePane),
        "C-w h" => Some(Action::FocusPane(PaneDir::Left)),
        "C-w j" => Some(Action::FocusPane(PaneDir::Down)),
        "C-w k" => Some(Action::FocusPane(PaneDir::Up)),
        "C-w l" => Some(Action::FocusPane(PaneDir::Right)),
        // Doom/Spacemacs SPC leader
        "SPC f s" => Some(Action::SaveFile),
        "SPC w s" => Some(Action::SplitHorizontal),
        "SPC w v" => Some(Action::SplitVertical),
        "SPC w w" => Some(Action::NextPane),
        "SPC w c" | "SPC w q" => Some(Action::ClosePane),
        "SPC w h" => Some(Action::FocusPane(PaneDir::Left)),
        "SPC w j" => Some(Action::FocusPane(PaneDir::Down)),
        "SPC w k" => Some(Action::FocusPane(PaneDir::Up)),
        "SPC w l" => Some(Action::FocusPane(PaneDir::Right)),
        "SPC q q" => Some(Action::QuitAll),
        _ => None,
    }
}

const KNOWN_PREFIXES: &[&str] = &[
    "g", "d", "y", "C-w", "SPC", "SPC f", "SPC w", "SPC b", "SPC q",
];

pub fn match_seq(seq_str: &str) -> MatchResult {
    if let Some(action) = exact_match(seq_str) {
        return MatchResult::Action(action);
    }
    if KNOWN_PREFIXES.contains(&seq_str) {
        return MatchResult::Prefix;
    }
    MatchResult::NoMatch
}

// ── Which-key hints ───────────────────────────────────────────────────────────

pub fn hint_for_prefix(prefix: &str) -> &'static str {
    match prefix {
        "g" => "g: top of file",
        "d" => "d: delete line  w: delete word",
        "y" => "y: yank line",
        "C-w" => "s: hsplit  v: vsplit  w: next  c/q: close  h/j/k/l: focus",
        "SPC" => "f: file  w: window  b: buffer  q: quit",
        "SPC f" => "s: save",
        "SPC w" => "s: hsplit  v: vsplit  w: next  c/q: close  h/j/k/l: focus",
        "SPC b" => "(buffer commands — coming soon)",
        "SPC q" => "q: quit",
        _ => "",
    }
}

// ── Key → string conversion ───────────────────────────────────────────────────

/// Produce a canonical string representation of a key event.
pub fn key_to_str(k: &KeyEvent) -> String {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Char(' ') if !ctrl => "SPC".into(),
        KeyCode::Char(c) if ctrl => format!("C-{}", c),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Enter => "RET".into(),
        KeyCode::Backspace => "BS".into(),
        KeyCode::Tab => "TAB".into(),
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        other => format!("{:?}", other),
    }
}

pub fn seq_to_str(seq: &[KeyEvent]) -> String {
    seq.iter().map(key_to_str).collect::<Vec<_>>().join(" ")
}
