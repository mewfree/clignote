use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, Mode};

/// Render the full editor UI into `frame`.
pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    // Layout: editor body + status bar (1 line) + command/message bar (1 line)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // editor
            Constraint::Length(1), // status bar
            Constraint::Length(1), // command / message
        ])
        .split(area);

    let editor_height = chunks[0].height as usize;
    app.scroll_to_cursor(editor_height);

    render_editor(frame, app, chunks[0]);
    render_status(frame, app, chunks[1]);
    render_cmdline(frame, app, chunks[2]);
}

// ── Editor pane ───────────────────────────────────────────────────────────────

fn render_editor(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let height = area.height as usize;
    let visible: Vec<Line> = (app.viewport_top..app.viewport_top + height)
        .map(|row| {
            match app.lines.get(row) {
                None => Line::default(),
                Some(line) => render_line(line, row, app),
            }
        })
        .collect();

    let para = Paragraph::new(visible);
    frame.render_widget(para, area);

    // Position the real terminal cursor
    let screen_row = (app.cursor_row - app.viewport_top) as u16 + area.y;
    let screen_col = app.cursor_col as u16 + area.x;
    frame.set_cursor_position((screen_col, screen_row));
}

/// Build one rendered line, applying org-mode-aware syntax highlighting.
fn render_line(line: &str, row: usize, app: &App) -> Line<'static> {
    let is_cursor_row = row == app.cursor_row;
    let spans = highlight_line(line, is_cursor_row, app.cursor_col, &app.mode);
    Line::from(spans)
}

fn highlight_line(
    line: &str,
    is_cursor_row: bool,
    cursor_col: usize,
    mode: &Mode,
) -> Vec<Span<'static>> {
    // Determine base style from line type
    let base_style = line_style(line);

    if !is_cursor_row || *mode == Mode::Insert {
        return vec![Span::styled(line.to_string(), base_style)];
    }

    // In Normal / Command mode: highlight the cursor cell
    if line.is_empty() {
        return vec![Span::styled(
            " ".to_string(),
            Style::default().bg(Color::White).fg(Color::Black),
        )];
    }

    let col = cursor_col.min(line.len().saturating_sub(1));
    let before = &line[..col];
    let cursor_char = line[col..].chars().next().unwrap_or(' ');
    let after_start = col + cursor_char.len_utf8();
    let after = &line[after_start..];

    let cursor_style = Style::default().bg(Color::White).fg(Color::Black);

    let mut spans = Vec::new();
    if !before.is_empty() {
        spans.push(Span::styled(before.to_string(), base_style));
    }
    spans.push(Span::styled(cursor_char.to_string(), cursor_style));
    if !after.is_empty() {
        spans.push(Span::styled(after.to_string(), base_style));
    }
    spans
}

/// Pick a display style based on org-mode line structure.
fn line_style(line: &str) -> Style {
    let level = line.chars().take_while(|&c| c == '*').count();
    if level > 0 && line.chars().nth(level) == Some(' ') {
        // Heading: colour by level
        let color = match level {
            1 => Color::LightBlue,
            2 => Color::LightGreen,
            3 => Color::LightYellow,
            4 => Color::LightMagenta,
            _ => Color::LightCyan,
        };
        return Style::default().fg(color).add_modifier(Modifier::BOLD);
    }
    if line.starts_with("#+") {
        return Style::default().fg(Color::DarkGray);
    }
    if (line.starts_with(':') && line.trim_end().ends_with(':'))
        || line.trim().eq_ignore_ascii_case(":end:")
    {
        return Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC);
    }
    Style::default()
}

// ── Status bar ────────────────────────────────────────────────────────────────

fn render_status(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let mode_style = match app.mode {
        Mode::Normal => Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD),
        Mode::Insert => Style::default().bg(Color::Green).fg(Color::Black).add_modifier(Modifier::BOLD),
        Mode::Command => Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD),
    };

    let mode_span = Span::styled(format!(" {} ", app.mode.label()), mode_style);

    let file_name = app
        .file_path
        .as_deref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("[no file]");
    let modified = if app.modified { " [+]" } else { "" };
    let file_span = Span::styled(
        format!(" {}{} ", file_name, modified),
        Style::default().fg(Color::White),
    );

    let pos = format!(" {}:{} ", app.cursor_row + 1, app.cursor_col + 1);
    let pos_span = Span::styled(pos, Style::default().fg(Color::DarkGray));

    let line = Line::from(vec![mode_span, file_span, pos_span]);
    let bar = Paragraph::new(line).style(Style::default().bg(Color::DarkGray));
    frame.render_widget(bar, area);
}

// ── Command / message line ────────────────────────────────────────────────────

fn render_cmdline(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let content = match app.mode {
        Mode::Command => format!(":{}", app.command_buf),
        _ => app.message.clone().unwrap_or_default(),
    };
    let para = Paragraph::new(content);
    frame.render_widget(para, area);
}
