use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Mode, SplitLayout};
use crate::pane::Pane;

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Reserve bottom two rows for status + command line
    let main_and_bars = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    let editor_area = main_and_bars[0];
    let status_area = main_and_bars[1];
    let cmd_area = main_and_bars[2];

    // Compute pane rects for this render pass and store them in App
    let pane_rects: Vec<Rect> = match app.layout {
        SplitLayout::Single => vec![editor_area],
        // Horizontal split (C-w s): panes stacked top/bottom — horizontal dividing line
        SplitLayout::Horizontal => {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(editor_area);
            vec![rows[0], rows[1]]
        }
        // Vertical split (C-w v): panes side by side — vertical dividing line
        SplitLayout::Vertical => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(editor_area);
            vec![cols[0], cols[1]]
        }
    };
    app.pane_rects = pane_rects.clone();

    // Render each pane
    for (i, &rect) in pane_rects.iter().enumerate() {
        let is_active = i == app.active_pane;
        let visual_sel = if is_active {
            app.visual_selection()
        } else {
            None
        };
        let mode = if is_active { &app.mode } else { &Mode::Normal };
        render_pane(frame, &mut app.panes[i], rect, is_active, visual_sel, mode);
    }

    // Draw a vertical divider between vertical-split (side by side) panes
    if app.layout == SplitLayout::Vertical && pane_rects.len() == 2 {
        let div_x = pane_rects[1].x;
        let div_style = Style::default().fg(Color::DarkGray);
        for y in editor_area.y..editor_area.y + editor_area.height {
            frame.render_widget(
                Paragraph::new("│").style(div_style),
                Rect::new(div_x.saturating_sub(1), y, 1, 1),
            );
        }
    }

    render_status(frame, app, status_area);
    render_cmdline(frame, app, cmd_area);
}

// ── Pane renderer ─────────────────────────────────────────────────────────────

fn render_pane(
    frame: &mut Frame,
    pane: &mut Pane,
    area: Rect,
    is_active: bool,
    visual_sel: Option<(usize, usize, usize, usize, bool)>,
    mode: &Mode,
) {
    let height = area.height as usize;
    pane.scroll_to_cursor(height);

    let visible: Vec<Line> = (pane.viewport_top..pane.viewport_top + height)
        .map(|row| match pane.lines.get(row) {
            None => Line::default(),
            Some(line) => build_line(
                line,
                row,
                pane.cursor_row,
                pane.cursor_col,
                visual_sel,
                mode,
                is_active,
            ),
        })
        .collect();

    frame.render_widget(Paragraph::new(visible), area);

    // Terminal cursor position (for blinking cursor in Insert)
    if is_active {
        let screen_row = area.y + (pane.cursor_row - pane.viewport_top) as u16;
        let screen_col = area.x + pane.cursor_col as u16;
        frame.set_cursor_position((screen_col, screen_row));
    }
}

// ── Line rendering ────────────────────────────────────────────────────────────

fn build_line(
    line: &str,
    row: usize,
    cursor_row: usize,
    cursor_col: usize,
    visual_sel: Option<(usize, usize, usize, usize, bool)>,
    mode: &Mode,
    is_active: bool,
) -> Line<'static> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let effective_n = n.max(1); // show at least a cursor cell on empty lines

    // Per-character base styles from org-mode analysis
    let base = org_styles(line);

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut cur_style = Style::default();

    for i in 0..effective_n {
        let ch = chars.get(i).copied().unwrap_or(' ');
        let base_style = base.get(i).copied().unwrap_or_default();

        let mut style = base_style;

        // Visual selection overlay
        if is_active {
            if let Some((sr, sc, er, ec, line_wise)) = visual_sel {
                if in_selection(row, i, sr, sc, er, ec, line_wise) {
                    style = style.bg(Color::Rgb(80, 80, 130)).fg(Color::White);
                }
            }
        }

        // Block cursor overlay (Normal / Command / Visual modes)
        if is_active && row == cursor_row && i == cursor_col && !matches!(mode, Mode::Insert) {
            style = Style::default().bg(Color::White).fg(Color::Black);
        }

        if style == cur_style {
            buf.push(ch);
        } else {
            if !buf.is_empty() {
                spans.push(Span::styled(buf.clone(), cur_style));
                buf.clear();
            }
            buf.push(ch);
            cur_style = style;
        }
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, cur_style));
    }

    Line::from(spans)
}

fn in_selection(
    row: usize,
    col: usize,
    sr: usize,
    sc: usize,
    er: usize,
    ec: usize,
    line_wise: bool,
) -> bool {
    if row < sr || row > er {
        return false;
    }
    if line_wise {
        return true;
    }
    if sr == er {
        return col >= sc && col <= ec;
    }
    if row == sr {
        return col >= sc;
    }
    if row == er {
        return col <= ec;
    }
    true
}

// ── Org-mode syntax styles ────────────────────────────────────────────────────

/// Return a per-character style array for one source line.
fn org_styles(line: &str) -> Vec<Style> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut styles = vec![Style::default(); n];
    if n == 0 {
        return styles;
    }

    // ── Heading ──────────────────────────────────────────────────────────────
    let star_count = chars.iter().take_while(|&&c| c == '*').count();
    if star_count > 0 && chars.get(star_count) == Some(&' ') {
        style_heading(&chars, &mut styles, star_count);
        return styles;
    }

    // ── #+keyword lines ───────────────────────────────────────────────────────
    if line.starts_with("#+") {
        let s = Style::default().fg(Color::DarkGray);
        styles.iter_mut().for_each(|st| *st = s);
        return styles;
    }

    // ── Drawer / property lines ───────────────────────────────────────────────
    if line.starts_with(':') {
        let s = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC);
        styles.iter_mut().for_each(|st| *st = s);
        return styles;
    }

    // ── Inline markup ─────────────────────────────────────────────────────────
    apply_link_styles(&chars, &mut styles);
    apply_strikethrough(&chars, &mut styles);

    // ── List bullets & checkboxes ─────────────────────────────────────────────
    if let Some(first_non_space) = chars.iter().position(|&c| c != ' ') {
        if matches!(chars[first_non_space], '-' | '+') {
            styles[first_non_space] = Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD);
            // Checkbox: "- [ ] " / "- [X] " / "- [-] "
            let cb_start = first_non_space + 2;
            if chars.get(cb_start) == Some(&'[') && chars.get(cb_start + 2) == Some(&']') {
                let state_char = chars.get(cb_start + 1).copied();
                let cb_style = match state_char {
                    Some('X') | Some('x') => Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                    Some('-') => Style::default().fg(Color::Yellow),
                    _ => Style::default().fg(Color::DarkGray),
                };
                for i in cb_start..=(cb_start + 2).min(n.saturating_sub(1)) {
                    styles[i] = cb_style;
                }
            }
        }
    }

    styles
}

/// Style `[[url]]` and `[[url][desc]]` link spans.
///
/// Bracket delimiters and the URL are dimmed; the description (or bare URL)
/// is rendered cyan + underlined so it stands out as a clickable reference.
fn apply_link_styles(chars: &[char], styles: &mut [Style]) {
    let n = chars.len();
    let dim = Style::default().fg(Color::DarkGray);
    let url_style = Style::default().fg(Color::DarkGray);
    let desc_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::UNDERLINED);

    let mut i = 0;
    while i + 1 < n {
        // Find opening "[["
        if chars[i] != '[' || chars[i + 1] != '[' {
            i += 1;
            continue;
        }
        let link_start = i;
        i += 2; // skip "[["

        // Collect URL until ']' or end
        let url_start = i;
        while i < n && chars[i] != ']' {
            i += 1;
        }
        if i >= n {
            break;
        }
        let url_end = i; // points at ']'

        i += 1; // skip first ']'
        if i >= n {
            break;
        }

        if chars[i] == ']' {
            // Form: [[url]]
            let link_end = i; // points at closing ']'
                              // Style: "[[" dim, url = desc_style, "]]" dim
            styles[link_start] = dim;
            styles[link_start + 1] = dim;
            for k in url_start..url_end {
                styles[k] = desc_style;
            }
            styles[url_end] = dim;
            styles[link_end] = dim;
            i += 1;
        } else if chars[i] == '[' {
            // Form: [[url][desc]]
            let sep_open = i; // '['
            i += 1;
            let desc_start = i;
            while i < n && chars[i] != ']' {
                i += 1;
            }
            if i >= n {
                break;
            }
            let desc_end = i; // first ']' of "]]"
            i += 1;
            if i >= n || chars[i] != ']' {
                continue;
            }
            let link_end = i;

            // Style: "[[" dim, url dim, "][" dim, desc cyan+underline, "]]" dim
            styles[link_start] = dim;
            styles[link_start + 1] = dim;
            for k in url_start..url_end {
                styles[k] = url_style;
            }
            styles[url_end] = dim; // ']' before '['
            styles[sep_open] = dim; // '['
            for k in desc_start..desc_end {
                styles[k] = desc_style;
            }
            styles[desc_end] = dim;
            styles[link_end] = dim;
            i += 1;
        }
    }
}

/// Apply `CROSSED_OUT` styling to `+text+` spans in a line.
/// Follows org-mode rules: opener must not be preceded by alphanumeric and must
/// not be followed by whitespace; closer must not be preceded by whitespace and
/// must not be followed by alphanumeric.
fn apply_strikethrough(chars: &[char], styles: &mut [Style]) {
    let n = chars.len();
    let mut i = 0;
    while i < n {
        if chars[i] != '+' {
            i += 1;
            continue;
        }
        let pre_ok = i == 0 || !chars[i - 1].is_alphanumeric();
        let content_start = i + 1;
        if !pre_ok || content_start >= n || chars[content_start].is_whitespace() {
            i += 1;
            continue;
        }
        // Search for closing '+'
        let mut j = content_start + 1;
        let mut found = false;
        while j < n {
            if chars[j] == '+' && !chars[j - 1].is_whitespace() {
                let post_ok = j + 1 >= n || !chars[j + 1].is_alphanumeric();
                if post_ok {
                    found = true;
                    break;
                }
            }
            j += 1;
        }
        if found {
            let strike_style = Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::CROSSED_OUT);
            for k in i..=j {
                styles[k] = strike_style;
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
}

fn style_heading(chars: &[char], styles: &mut [Style], star_count: usize) {
    let n = chars.len();
    let level_color = heading_color(star_count);
    let dim = Style::default().fg(Color::DarkGray);
    let title_style = Style::default()
        .fg(level_color)
        .add_modifier(Modifier::BOLD);

    // Stars: dimmed
    for i in 0..star_count.min(n) {
        styles[i] = dim;
    }
    // Space after stars
    if star_count < n {
        styles[star_count] = title_style;
    }

    let mut pos = star_count + 1;

    // TODO keyword
    const KEYWORDS: &[(&str, Color, bool)] = &[
        ("TODO", Color::LightRed, true),
        ("NEXT", Color::LightYellow, true),
        ("DOING", Color::LightYellow, true),
        ("WAITING", Color::LightMagenta, false),
        ("HOLD", Color::LightMagenta, false),
        ("DONE", Color::Green, false),
        ("CANCELLED", Color::DarkGray, false),
    ];
    for &(kw, color, bold) in KEYWORDS {
        let kw_chars: Vec<char> = kw.chars().collect();
        let kw_len = kw_chars.len();
        if pos + kw_len <= n && chars[pos..pos + kw_len] == kw_chars[..] {
            let next = chars.get(pos + kw_len).copied();
            if next == Some(' ') || next.is_none() {
                let mut kw_style = Style::default().fg(color);
                if bold {
                    kw_style = kw_style.add_modifier(Modifier::BOLD);
                }
                for i in pos..pos + kw_len {
                    styles[i] = kw_style;
                }
                pos += kw_len;
                if pos < n && chars[pos] == ' ' {
                    styles[pos] = title_style;
                    pos += 1;
                }
                break;
            }
        }
    }

    // Priority [#A] / [#B] / [#C]
    if pos + 4 <= n && chars[pos] == '[' && chars[pos + 1] == '#' && chars[pos + 3] == ']' {
        let p = chars[pos + 2];
        let pri_color = match p {
            'A' => Color::LightRed,
            'B' => Color::LightYellow,
            'C' => Color::LightGreen,
            _ => Color::White,
        };
        let pri_style = Style::default().fg(pri_color).add_modifier(Modifier::BOLD);
        for i in pos..pos + 4 {
            styles[i] = pri_style;
        }
        pos += 4;
        if pos < n && chars[pos] == ' ' {
            styles[pos] = title_style;
            pos += 1;
        }
    }

    // Tags at end: :foo:bar:
    let tag_start = find_tags_char_col(chars, pos);

    // Title range
    for i in pos..tag_start.min(n) {
        styles[i] = title_style;
    }
    // Tags range
    for i in tag_start..n {
        styles[i] = dim;
    }
}

/// Find the char index of the ':' that opens the trailing tag section,
/// or `chars.len()` if there are no tags.
fn find_tags_char_col(chars: &[char], from: usize) -> usize {
    let n = chars.len();
    if n == 0 || chars[n - 1] != ':' {
        return n;
    }
    // Walk backwards: collect tag segments between colons
    let mut pos = n - 1; // position of last ':'
    loop {
        let colon_pos = pos;
        if colon_pos == 0 {
            return n;
        }
        pos -= 1;
        let seg_end = colon_pos;
        // Scan tag chars backward
        let mut seg_start = colon_pos;
        while seg_start > 0 && is_tag_char(chars[seg_start - 1]) {
            seg_start -= 1;
        }
        let tag = &chars[seg_start..seg_end];
        if tag.is_empty() {
            return n;
        }
        if seg_start <= from {
            return n;
        }
        let before_tag = chars[seg_start - 1];
        if before_tag == ' ' || before_tag == '\t' {
            // The ':' at seg_start - 1 is the opening of the tag section
            // But wait: seg_start - 1 is a space, not a colon.
            // In ":foo:bar:", the opening ':' would be right before "foo".
            // Hmm, let me trace: "Title :foo:bar:"
            //   chars[...] has space at some position, then ':' at seg_start
            //   but seg_start is the start of tag text, not the colon...
            // This means `seg_start` is at 'f' of "foo", and chars[seg_start-1]
            // should be ':' for a valid tag.
            return n; // space before tag text, not a colon — invalid
        }
        if before_tag == ':' {
            // seg_start - 1 is a ':' → valid tag separator, keep walking
            let _ = pos; // suppress unused assignment warning
            pos = seg_start - 1;
            // Check what is before this ':'
            if pos == 0 {
                return n;
            }
            let before_colon = chars[pos - 1];
            if before_colon == ' ' || before_colon == '\t' || pos - 1 < from {
                // Found the opening ':' of the whole tag section
                if pos - 1 >= from {
                    return pos;
                }
                return n;
            }
        } else {
            return n;
        }
    }
}

fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '@' | '#' | '%')
}

fn heading_color(level: usize) -> Color {
    match level {
        1 => Color::LightBlue,
        2 => Color::LightGreen,
        3 => Color::LightYellow,
        4 => Color::LightMagenta,
        _ => Color::LightCyan,
    }
}

// ── Status bar ────────────────────────────────────────────────────────────────

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let mode_style = match app.mode {
        Mode::Normal => Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        Mode::Insert => Style::default()
            .bg(Color::Green)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
        Mode::Command => Style::default()
            .bg(Color::Yellow)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
        Mode::Visual { .. } => Style::default()
            .bg(Color::Magenta)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    };

    let pane = app.pane();
    let mode_span = Span::styled(format!(" {} ", app.mode.label()), mode_style);

    let file_name = pane
        .file_path
        .as_deref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("[no file]");
    let modified = if pane.modified { " [+]" } else { "" };
    let split_indicator = match app.layout {
        SplitLayout::Single => "",
        SplitLayout::Horizontal => "  [hsplit]",
        SplitLayout::Vertical => "  [vsplit]",
    };

    let file_span = Span::styled(
        format!(" {}{}{} ", file_name, modified, split_indicator),
        Style::default().fg(Color::White),
    );

    let pos_span = Span::styled(
        format!(" {}:{} ", pane.cursor_row + 1, pane.cursor_col + 1),
        Style::default().fg(Color::DarkGray),
    );

    let bar = Paragraph::new(Line::from(vec![mode_span, file_span, pos_span]))
        .style(Style::default().bg(Color::DarkGray));
    frame.render_widget(bar, area);
}

// ── Command / message / which-key line ────────────────────────────────────────

fn render_cmdline(frame: &mut Frame, app: &App, area: Rect) {
    let content = match app.mode {
        Mode::Command => format!(":{}", app.command_buf),
        _ => app.message.clone().unwrap_or_default(),
    };
    frame.render_widget(Paragraph::new(content), area);
}
