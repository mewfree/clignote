use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Mode, SplitLayout};
use crate::git::LineStatus;
use crate::pane::Pane;

struct PaneRenderCtx<'a> {
    is_active: bool,
    visual_sel: Option<(usize, usize, usize, usize, bool)>,
    mode: &'a Mode,
    conceal_links: bool,
}

struct SearchView<'a> {
    matches: &'a [(usize, usize)],
    query_len: usize,
    current_idx: Option<usize>,
}

struct LineCtx<'a> {
    cursor_row: usize,
    cursor_col: usize,
    visual_sel: Option<(usize, usize, usize, usize, bool)>,
    mode: &'a Mode,
    is_active: bool,
    search_spans: &'a [(usize, usize, bool)],
    conceal_links: bool,
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Reserve bottom rows: status bar + cmdline (like NeoVim)
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
    let cmdline_area = main_and_bars[2];

    if app.mode == Mode::Browse {
        render_browse(frame, app, editor_area);
        render_status(frame, app, status_area);
        render_cmdline(frame, app, cmdline_area);
        return;
    }

    if app.mode == Mode::BufferList {
        render_buffer_list(frame, app, editor_area);
        render_status(frame, app, status_area);
        render_cmdline(frame, app, cmdline_area);
        return;
    }

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

    // Snapshot search state so we can borrow panes mutably inside the loop.
    let search_query_len = app.search_buf.len();
    let search_matches: Vec<(usize, usize)> = app.search_matches.clone();
    let search_match_idx = app.search_match_idx;

    // Render each pane
    for (i, &rect) in pane_rects.iter().enumerate() {
        let is_active = i == app.active_pane;
        let visual_sel = if is_active {
            app.visual_selection()
        } else {
            None
        };
        let mode = if is_active { &app.mode } else { &Mode::Normal };
        let search = if is_active {
            SearchView {
                matches: &search_matches,
                query_len: search_query_len,
                current_idx: Some(search_match_idx),
            }
        } else {
            SearchView {
                matches: &[],
                query_len: 0,
                current_idx: None,
            }
        };
        let pane_ctx = PaneRenderCtx {
            is_active,
            visual_sel,
            mode,
            conceal_links: app.conceal_links,
        };
        render_pane(frame, &mut app.panes[i], rect, &pane_ctx, &search);
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
    render_cmdline(frame, app, cmdline_area);
}

// ── Directory browser renderer ─────────────────────────────────────────────────

fn render_browse(frame: &mut Frame, app: &mut App, area: Rect) {
    const HEADER_ROWS: usize = 2; // path line + blank line

    let list_height = (area.height as usize).saturating_sub(HEADER_ROWS);
    if list_height > 0 {
        if app.browse_selected < app.browse_scroll {
            app.browse_scroll = app.browse_selected;
        } else if app.browse_selected >= app.browse_scroll + list_height {
            app.browse_scroll = app.browse_selected - list_height + 1;
        }
    }

    let header = Line::from(Span::styled(
        app.browse_dir.display().to_string(),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ));

    let mut lines: Vec<Line> = vec![header, Line::default()];
    if app.browse_entries.is_empty() {
        lines.push(Line::from(Span::styled(
            " (no directories or .org files) ",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let end = (app.browse_scroll + list_height).min(app.browse_entries.len());
        for (i, entry) in app.browse_entries[app.browse_scroll..end]
            .iter()
            .enumerate()
        {
            let idx = app.browse_scroll + i;
            let selected = idx == app.browse_selected;
            let label = if entry.is_dir {
                format!("{}/", entry.name)
            } else {
                entry.name.clone()
            };
            let mut style = if entry.is_dir {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            if selected {
                style = style.bg(Color::White).fg(Color::Black);
            }
            lines.push(Line::from(Span::styled(format!(" {} ", label), style)));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_buffer_list(frame: &mut Frame, app: &mut App, area: Rect) {
    const HEADER_ROWS: usize = 2; // title line + blank line

    let list_height = (area.height as usize).saturating_sub(HEADER_ROWS);
    if list_height > 0 {
        if app.buf_list_selected < app.buf_list_scroll {
            app.buf_list_scroll = app.buf_list_selected;
        } else if app.buf_list_selected >= app.buf_list_scroll + list_height {
            app.buf_list_scroll = app.buf_list_selected - list_height + 1;
        }
    }

    let header = Line::from(Span::styled(
        "Buffers  (RET open, d kill, Esc cancel)",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ));

    let active_path = app.pane().file_path.clone();
    let mut lines: Vec<Line> = vec![header, Line::default()];
    if app.buffers.is_empty() {
        lines.push(Line::from(Span::styled(
            " (no buffers) ",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let end = (app.buf_list_scroll + list_height).min(app.buffers.len());
        for (i, path) in app.buffers[app.buf_list_scroll..end].iter().enumerate() {
            let idx = app.buf_list_scroll + i;
            let selected = idx == app.buf_list_selected;
            let is_active = active_path.as_deref() == Some(path.as_path());
            let marker = if is_active { "* " } else { "  " };
            let mut style = if is_active {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            if selected {
                style = style.bg(Color::White).fg(Color::Black);
            }
            lines.push(Line::from(Span::styled(
                format!(" {}{} ", marker, path.display()),
                style,
            )));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

// ── Pane renderer ─────────────────────────────────────────────────────────────

fn render_pane(
    frame: &mut Frame,
    pane: &mut Pane,
    area: Rect,
    pane_ctx: &PaneRenderCtx<'_>,
    search: &SearchView<'_>,
) {
    let is_active = pane_ctx.is_active;
    let height = area.height as usize;

    // Split off a 1-column gutter on the left for git status marks.
    const GUTTER: u16 = 1;
    let (gutter_area, content_area) = if area.width > GUTTER {
        (
            Rect::new(area.x, area.y, GUTTER, area.height),
            Rect::new(area.x + GUTTER, area.y, area.width - GUTTER, area.height),
        )
    } else {
        (Rect::new(area.x, area.y, 0, 0), area)
    };

    let width = content_area.width as usize;
    pane.scroll_to_cursor(height, width);
    pane.recompute_git_diff();

    // Render git gutter
    let gutter_lines: Vec<Line> = (pane.viewport_top..pane.viewport_top + height)
        .map(|row| {
            let status = pane.git_diff.get(row).copied().unwrap_or_default();
            let (ch, style) = match status {
                LineStatus::Added => ('+', Style::default().fg(Color::Green)),
                LineStatus::Modified => ('~', Style::default().fg(Color::Yellow)),
                LineStatus::Unchanged => (' ', Style::default()),
            };
            Line::from(Span::styled(ch.to_string(), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(gutter_lines), gutter_area);

    // Render content (full lines; horizontal scroll applied via Paragraph)
    let visible: Vec<Line> = (pane.viewport_top..pane.viewport_top + height)
        .map(|row| match pane.lines.get(row) {
            None => Line::default(),
            Some(line) => {
                let row_search: Vec<(usize, usize, bool)> = search
                    .matches
                    .iter()
                    .enumerate()
                    .filter_map(|(mi, &(r, c))| {
                        if r == row && search.query_len > 0 {
                            Some((c, search.query_len, search.current_idx == Some(mi)))
                        } else {
                            None
                        }
                    })
                    .collect();
                let ctx = LineCtx {
                    cursor_row: pane.cursor_row,
                    cursor_col: pane.cursor_col,
                    visual_sel: pane_ctx.visual_sel,
                    mode: pane_ctx.mode,
                    is_active,
                    search_spans: &row_search,
                    // Reveal raw link syntax only while actively editing that line in
                    // Insert mode; Normal-mode navigation never pops links open.
                    conceal_links: pane_ctx.conceal_links
                        && !(is_active
                            && row == pane.cursor_row
                            && matches!(pane_ctx.mode, Mode::Insert)),
                };
                build_line(line, row, &ctx)
            }
        })
        .collect();

    frame.render_widget(
        Paragraph::new(visible).scroll((0, pane.viewport_left as u16)),
        content_area,
    );

    // Terminal cursor position (for blinking cursor in Insert)
    if is_active {
        let screen_row = content_area.y + (pane.cursor_row - pane.viewport_top) as u16;
        let screen_col = content_area.x + (pane.cursor_col - pane.viewport_left) as u16;
        frame.set_cursor_position((screen_col, screen_row));
    }
}

// ── Line rendering ────────────────────────────────────────────────────────────

fn build_line(line: &str, row: usize, ctx: &LineCtx<'_>) -> Line<'static> {
    let (chars, base) = if ctx.conceal_links {
        conceal_links(line)
    } else {
        (line.chars().collect(), org_styles(line))
    };
    let n = chars.len();
    let effective_n = n.max(1); // show at least a cursor cell on empty lines

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut cur_style = Style::default();

    for i in 0..effective_n {
        let ch = chars.get(i).copied().unwrap_or(' ');
        let base_style = base.get(i).copied().unwrap_or_default();

        let mut style = base_style;

        // Visual selection overlay
        if ctx.is_active {
            if let Some((sr, sc, er, ec, line_wise)) = ctx.visual_sel {
                if in_selection(row, i, sr, sc, er, ec, line_wise) {
                    style = style.bg(Color::Rgb(80, 80, 130)).fg(Color::White);
                }
            }
        }

        // Search highlight overlay
        for &(col_start, len, is_current) in ctx.search_spans {
            if i >= col_start && i < col_start + len {
                if is_current {
                    style = Style::default().bg(Color::Yellow).fg(Color::Black);
                } else {
                    style = Style::default().bg(Color::Rgb(80, 60, 0)).fg(Color::White);
                }
                break;
            }
        }

        // Block cursor overlay (Normal / Command / Visual modes)
        if ctx.is_active
            && row == ctx.cursor_row
            && i == ctx.cursor_col
            && !matches!(ctx.mode, Mode::Insert)
        {
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
                styles[cb_start..=(cb_start + 2).min(n.saturating_sub(1))].fill(cb_style);
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
            styles[url_start..url_end].fill(desc_style);
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
            styles[url_start..url_end].fill(url_style);
            styles[url_end] = dim; // ']' before '['
            styles[sep_open] = dim; // '['
            styles[desc_start..desc_end].fill(desc_style);
            styles[desc_end] = dim;
            styles[link_end] = dim;
            i += 1;
        }
    }
}

/// Build the display chars/styles for a line with link syntax concealed:
/// `[[url][desc]]` collapses to just `desc`, and `[[url]]` collapses to just
/// `url`. All other styling (headings, strikethrough, etc.) is unaffected.
fn conceal_links(line: &str) -> (Vec<char>, Vec<Style>) {
    let chars: Vec<char> = line.chars().collect();
    let styles = org_styles(line);
    let n = chars.len();

    // Ranges of source-char indices to drop from the display (brackets, "][", URL).
    let mut drop: Vec<(usize, usize)> = Vec::new();

    let mut i = 0;
    while i + 1 < n {
        if chars[i] != '[' || chars[i + 1] != '[' {
            i += 1;
            continue;
        }
        let link_start = i;
        i += 2;
        let url_start = i;
        while i < n && chars[i] != ']' {
            i += 1;
        }
        if i >= n {
            break;
        }
        let url_end = i;
        i += 1;
        if i >= n {
            break;
        }

        if chars[i] == ']' {
            // [[url]] — drop "[[" and "]]", keep url text.
            drop.push((link_start, url_start));
            drop.push((url_end, i + 1));
            i += 1;
        } else if chars[i] == '[' {
            // [[url][desc]] — drop everything but desc text.
            i += 1;
            let desc_start = i;
            while i < n && chars[i] != ']' {
                i += 1;
            }
            if i >= n {
                break;
            }
            let desc_end = i;
            i += 1;
            if i >= n || chars[i] != ']' {
                continue;
            }
            drop.push((link_start, desc_start));
            drop.push((desc_end, i + 1));
            i += 1;
        }
    }

    if drop.is_empty() {
        return (chars, styles);
    }

    let mut out_chars = Vec::with_capacity(n);
    let mut out_styles = Vec::with_capacity(n);
    'outer: for idx in 0..n {
        for &(start, end) in &drop {
            if idx >= start && idx < end {
                continue 'outer;
            }
        }
        out_chars.push(chars[idx]);
        out_styles.push(styles[idx]);
    }
    (out_chars, out_styles)
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
            styles[i..=j].fill(strike_style);
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
    styles[..star_count.min(n)].fill(dim);
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
                styles[pos..pos + kw_len].fill(kw_style);
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
        styles[pos..pos + 4].fill(pri_style);
        pos += 4;
        if pos < n && chars[pos] == ' ' {
            styles[pos] = title_style;
            pos += 1;
        }
    }

    // Tags at end: :foo:bar:
    let tag_start = find_tags_char_col(chars, pos);

    // Title range
    styles[pos..tag_start.min(n)].fill(title_style);
    // Tags range
    styles[tag_start..n].fill(dim);
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
                if pos > from {
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
        Mode::Search => Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
        Mode::Visual { .. } => Style::default()
            .bg(Color::Magenta)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        Mode::Browse => Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
        Mode::BufferList => Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
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

fn render_cmdline(frame: &mut Frame, app: &App, area: Rect) {
    let text = match app.mode {
        Mode::Command => format!(":{}", app.command_buf),
        Mode::Search => {
            let n = app.search_matches.len();
            if app.search_buf.is_empty() {
                "/".to_string()
            } else if n == 0 {
                format!("/{} (no matches)", app.search_buf)
            } else {
                format!("/{} ({}/{})", app.search_buf, app.search_match_idx + 1, n)
            }
        }
        _ => app
            .message
            .as_deref()
            .map(|m| m.to_string())
            .unwrap_or_default(),
    };
    let widget = Paragraph::new(text).style(Style::default().fg(Color::White));
    frame.render_widget(widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conceal_links_shows_description_only() {
        let (chars, _) = conceal_links("see [[https://example.com][the docs]] here");
        let s: String = chars.into_iter().collect();
        assert_eq!(s, "see the docs here");
    }

    #[test]
    fn conceal_links_shows_bare_url_when_no_description() {
        let (chars, _) = conceal_links("see [[https://example.com]] here");
        let s: String = chars.into_iter().collect();
        assert_eq!(s, "see https://example.com here");
    }

    #[test]
    fn conceal_links_leaves_plain_text_untouched() {
        let (chars, _) = conceal_links("no links here");
        let s: String = chars.into_iter().collect();
        assert_eq!(s, "no links here");
    }
}
