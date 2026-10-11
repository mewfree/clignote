use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::structure::StructureOp;

// ── Action ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneDir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    GoToFileStart,
    DeleteLine,
    DeleteWord,
    YankLine,
    SplitHorizontal,
    SplitVertical,
    ClosePane,
    NextPane,
    FocusPane(PaneDir),
    SaveFile,
    QuitAll,
    ToggleLinkConceal,
    BufferList,
    NextBuffer,
    PrevBuffer,
    KillBuffer,
    /// Org heading / TODO editing (`SPC m …`, `M-h/j/k/l`).
    Structure(StructureOp),
}

// ── Sequence matching ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        "SPC t l" => Some(Action::ToggleLinkConceal),
        // Doom/Spacemacs SPC b buffer commands
        "SPC b b" => Some(Action::BufferList),
        "SPC b n" => Some(Action::NextBuffer),
        "SPC b p" => Some(Action::PrevBuffer),
        "SPC b d" | "SPC b k" => Some(Action::KillBuffer),
        // Org structure editing (Doom's `SPC m` local leader)
        "SPC m h" => Some(Action::Structure(StructureOp::Promote)),
        "SPC m l" => Some(Action::Structure(StructureOp::Demote)),
        "SPC m H" => Some(Action::Structure(StructureOp::PromoteSubtree)),
        "SPC m L" => Some(Action::Structure(StructureOp::DemoteSubtree)),
        "SPC m k" => Some(Action::Structure(StructureOp::MoveUp)),
        "SPC m j" => Some(Action::Structure(StructureOp::MoveDown)),
        "SPC m t" => Some(Action::Structure(StructureOp::TodoNext)),
        "SPC m T" => Some(Action::Structure(StructureOp::TodoPrev)),
        _ => None,
    }
}

const KNOWN_PREFIXES: &[&str] = &[
    "g", "d", "y", "C-w", "SPC", "SPC f", "SPC w", "SPC b", "SPC m", "SPC q", "SPC t",
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
        "SPC" => "f: file  w: window  b: buffer  m: org  q: quit  t: toggle",
        "SPC m" => "h/l: promote/demote  H/L: subtree  j/k: move  t/T: TODO",
        "SPC f" => "s: save",
        "SPC w" => "s: hsplit  v: vsplit  w: next  c/q: close  h/j/k/l: focus",
        "SPC b" => "b: list  n: next  p: prev  d/k: kill",
        "SPC q" => "q: quit",
        "SPC t" => "l: toggle link display (label/target)",
        _ => "",
    }
}

// ── Key → string conversion ───────────────────────────────────────────────────

/// Produce a canonical string representation of a key event.
pub fn key_to_str(k: &KeyEvent) -> String {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    match k.code {
        KeyCode::Char(c) if alt => format!("M-{c}"),
        KeyCode::Char(' ') if !ctrl => "SPC".into(),
        KeyCode::Char(c) if ctrl => format!("C-{c}"),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Enter => "RET".into(),
        KeyCode::Backspace => "BS".into(),
        KeyCode::Tab => "TAB".into(),
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        other => format!("{other:?}"),
    }
}

pub fn seq_to_str(seq: &[KeyEvent]) -> String {
    seq.iter().map(key_to_str).collect::<Vec<_>>().join(" ")
}
