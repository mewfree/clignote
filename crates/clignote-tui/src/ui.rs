use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, LineNumbers, Mode, PaneLayout, SplitDir};
use crate::git::LineStatus;
use crate::links;
use crate::pane::{scroll_into_view, Pane};
use crate::wrap::{self, WrapState};

type VisualSel = (usize, usize, usize, usize, bool);

struct PaneRenderCtx {
    is_active: bool,
    visual_sel: Option<VisualSel>,
    mode: Mode,
    conceal_links: bool,
    /// Soft-wrap long lines instead of scrolling horizontally.
    wrap: bool,
    /// Indent body text under headings.
    indent: bool,
    line_numbers: LineNumbers,
}

struct SearchView<'a> {
    matches: &'a [(usize, usize)],
    query_len: usize,
    current_idx: Option<usize>,
}

struct LineCtx<'a> {
    cursor_row: usize,
    cursor_col: usize,
    visual_sel: Option<VisualSel>,
    mode: Mode,
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

    // Compute pane rects for this render pass and store them in App, indexed
    // by pane index (not tree position) so click/focus lookups stay simple.
    app.pane_rects = vec![Rect::default(); app.panes.len()];
    layout_rects(&app.layout, editor_area, &mut app.pane_rects);

    // In Browse/BufferList mode, the active pane's content is replaced by the
    // overlay; other panes keep showing their buffers.
    let overlay_active = matches!(app.mode, Mode::Browse | Mode::BufferList);
    let visual_sel = app.visual_selection();

    // Render each pane. Fields are borrowed disjointly, so the search matches
    // can be read in place while the panes are mutated.
    for i in 0..app.panes.len() {
        let is_active = i == app.active_pane;
        if is_active && overlay_active {
            continue;
        }
        let search = if is_active {
            SearchView {
                matches: &app.search_matches,
                query_len: app.search_buf.len(),
                current_idx: Some(app.search_match_idx),
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
            visual_sel: visual_sel.filter(|_| is_active),
            mode: if is_active { app.mode } else { Mode::Normal },
            conceal_links: app.conceal_links,
            wrap: app.wrap,
            indent: app.indent,
            line_numbers: app.line_numbers,
        };
        render_pane(
            frame,
            &mut app.panes[i],
            app.pane_rects[i],
            &pane_ctx,
            &search,
        );
    }

    let active_rect = app.pane_rects[app.active_pane];
    match app.mode {
        Mode::Browse => render_browse(frame, app, active_rect),
        Mode::BufferList => render_buffer_list(frame, app, active_rect),
        _ => {}
    }

    draw_dividers(frame, &app.layout, editor_area);

    render_status(frame, app, status_area);
    render_cmdline(frame, app, cmdline_area);
}

/// Divide `area` equally between `n` children of a split.
fn split_rects(dir: SplitDir, n: usize, area: Rect) -> Vec<Rect> {
    let direction = match dir {
        SplitDir::Horizontal => Direction::Vertical, // stacked top/bottom
        SplitDir::Vertical => Direction::Horizontal, // side by side
    };
    Layout::new(direction, vec![Constraint::Fill(1); n])
        .split(area)
        .to_vec()
}

/// Recursively split `area` per the window layout tree, writing each pane's
/// rect into `out[pane_idx]`.
fn layout_rects(layout: &PaneLayout, area: Rect, out: &mut [Rect]) {
    match layout {
        PaneLayout::Leaf(idx) => out[*idx] = area,
        PaneLayout::Split { dir, children } => {
            let rects = split_rects(*dir, children.len(), area);
            for (child, rect) in children.iter().zip(rects) {
                layout_rects(child, rect, out);
            }
        }
    }
}

/// Recursively draw a divider line at each split boundary in the window
/// layout tree (vertical `│` between side-by-side panes, horizontal `─`
/// between stacked panes).
fn draw_dividers(frame: &mut Frame, layout: &PaneLayout, area: Rect) {
    let PaneLayout::Split { dir, children } = layout else {
        return;
    };
    let rects = split_rects(*dir, children.len(), area);

    let div_style = Style::default().fg(Color::DarkGray);
    for rect in rects.iter().skip(1) {
        match dir {
            SplitDir::Vertical => {
                let x = rect.x.saturating_sub(1);
                let column = vec![Line::from("│"); area.height as usize];
                frame.render_widget(
                    Paragraph::new(column).style(div_style),
                    Rect::new(x, area.y, 1, area.height),
                );
            }
            SplitDir::Horizontal => {
                let y = rect.y.saturating_sub(1);
                frame.render_widget(
                    Paragraph::new("─".repeat(area.width as usize)).style(div_style),
                    Rect::new(area.x, y, area.width, 1),
                );
            }
        }
    }

    for (child, rect) in children.iter().zip(rects) {
        draw_dividers(frame, child, rect);
    }
}

// ── Directory browser renderer ─────────────────────────────────────────────────

/// Style for a row of a selectable overlay list.
fn list_row_style(base: Style, selected: bool) -> Style {
    if selected {
        base.bg(Color::White).fg(Color::Black)
    } else {
        base
    }
}

fn render_browse(frame: &mut Frame, app: &mut App, area: Rect) {
    const HEADER_ROWS: usize = 2; // path line + blank line

    let list_height = (area.height as usize).saturating_sub(HEADER_ROWS);
    scroll_into_view(&mut app.browse_scroll, app.browse_selected, list_height);

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
        let rows = app
            .browse_entries
            .iter()
            .enumerate()
            .skip(app.browse_scroll)
            .take(list_height);
        for (idx, entry) in rows {
            let (label, base) = if entry.is_dir {
                (format!("{}/", entry.name), Style::default().fg(Color::Cyan))
            } else {
                (entry.name.clone(), Style::default())
            };
            let style = list_row_style(base, idx == app.browse_selected);
            lines.push(Line::from(Span::styled(format!(" {label} "), style)));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_buffer_list(frame: &mut Frame, app: &mut App, area: Rect) {
    const HEADER_ROWS: usize = 2; // title line + blank line

    let list_height = (area.height as usize).saturating_sub(HEADER_ROWS);
    scroll_into_view(&mut app.buf_list_scroll, app.buf_list_selected, list_height);

    let header = Line::from(Span::styled(
        "Buffers  (RET open, d kill, Esc cancel)",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ));

    let active_path = app.pane().file_path.as_deref();
    let mut lines: Vec<Line> = vec![header, Line::default()];
    if app.buffers.is_empty() {
        lines.push(Line::from(Span::styled(
            " (no buffers) ",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let rows = app
            .buffers
            .iter()
            .enumerate()
            .skip(app.buf_list_scroll)
            .take(list_height);
        for (idx, path) in rows {
            let is_active = active_path == Some(path.as_path());
            let marker = if is_active { "* " } else { "  " };
            let base = if is_active {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            let style = list_row_style(base, idx == app.buf_list_selected);
            lines.push(Line::from(Span::styled(
                format!(" {marker}{} ", path.display()),
                style,
            )));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

const GIT_GUTTER: u16 = 1;

/// Width of the line-number column (digits plus a separating space), or 0.
fn number_width(mode: LineNumbers, line_count: usize) -> u16 {
    if mode == LineNumbers::Off {
        return 0;
    }
    let digits = line_count.max(1).to_string().len() as u16;
    digits.max(2) + 1
}

/// Total gutter width for a pane of `area_width`: git column plus line
/// numbers, dropped when the pane is too narrow to leave room for content.
pub fn gutter_width(mode: LineNumbers, line_count: usize, area_width: u16) -> u16 {
    let full = GIT_GUTTER + number_width(mode, line_count);
    if area_width > full {
        full
    } else if area_width > GIT_GUTTER {
        GIT_GUTTER
    } else {
        0
    }
}

// ── Pane renderer ─────────────────────────────────────────────────────────────

fn render_pane(
    frame: &mut Frame,
    pane: &mut Pane,
    area: Rect,
    pane_ctx: &PaneRenderCtx,
    search: &SearchView<'_>,
) {
    let is_active = pane_ctx.is_active;
    let height = area.height as usize;

    // Split off a gutter on the left: 1 column of git status marks, then
    // the line-number column when enabled.
    let num_width = number_width(pane_ctx.line_numbers, pane.lines.len());
    let gutter = gutter_width(pane_ctx.line_numbers, pane.lines.len(), area.width);
    let num_width = if gutter > GIT_GUTTER { num_width } else { 0 };
    let (gutter_area, content_area) = if gutter > 0 {
        (
            Rect::new(area.x, area.y, gutter, area.height),
            Rect::new(area.x + gutter, area.y, area.width - gutter, area.height),
        )
    } else {
        (Rect::new(area.x, area.y, 0, 0), area)
    };

    let width = content_area.width as usize;
    let insert_here = is_active && matches!(pane_ctx.mode, Mode::Insert);
    pane.set_wrap((pane_ctx.wrap && width > 0).then_some(WrapState {
        width,
        conceal_links: pane_ctx.conceal_links,
        insert: insert_here,
    }));
    pane.set_indent(pane_ctx.indent);
    pane.scroll_to_cursor(height, width);
    pane.recompute_git_diff();

    // Screen rows as (buffer row, wrapped segment), skipping folded regions.
    let fold_ranges = pane.fold_ranges();
    let layout = pane.screen_layout(&fold_ranges, height);
    let wrapping = pane.is_wrapping();

    let mut gutter_lines: Vec<Line> = Vec::with_capacity(layout.len());
    let mut visible: Vec<Line> = Vec::with_capacity(layout.len());
    let mut i = 0;
    while i < layout.len() {
        let row = layout[i].0;
        let run_len = layout[i..].iter().take_while(|&&(r, _)| r == row).count();

        // Matches are sorted by (row, col): binary-search this row's run.
        let lo = search.matches.partition_point(|&(r, _)| r < row);
        let hi = lo + search.matches[lo..].partition_point(|&(r, _)| r == row);
        let row_search: Vec<(usize, usize, bool)> = if search.query_len > 0 {
            (lo..hi)
                .map(|mi| {
                    let col = search.matches[mi].1;
                    (col, search.query_len, search.current_idx == Some(mi))
                })
                .collect()
        } else {
            Vec::new()
        };
        let ctx = LineCtx {
            cursor_row: pane.cursor_row,
            cursor_col: pane.cursor_col,
            visual_sel: pane_ctx.visual_sel,
            mode: pane_ctx.mode,
            is_active,
            search_spans: &row_search,
            // Reveal raw link syntax only while actively editing that line in
            // Insert mode; Normal-mode navigation never pops links open.
            conceal_links: pane_ctx.conceal_links && !(insert_here && row == pane.cursor_row),
        };
        let cells = build_cells(&pane.lines[row], row, &ctx);
        let segs = if wrapping {
            pane.row_segments(row)
        } else {
            wrap::whole(cells.len())
        };

        let status = pane.git_diff.get(row).copied().unwrap_or_default();
        let indent = pane.row_indent(row);
        for &(_, seg) in &layout[i..i + run_len] {
            let range = segs.get(seg).cloned().unwrap_or(0..0);
            let (start, end) = (range.start.min(cells.len()), range.end.min(cells.len()));
            let mut line = Line::from(spans_from(&cells[start..end]));
            if indent > 0 {
                line.spans.insert(0, Span::raw(" ".repeat(indent)));
            }
            if seg + 1 == segs.len() && Pane::is_fold_start(&fold_ranges, row) {
                line.spans
                    .push(Span::styled(" […]", Style::default().fg(Color::DarkGray)));
            }
            visible.push(line);

            // Git mark on the first screen row of a buffer row only.
            let (ch, style) = match status {
                _ if seg > 0 => (" ", Style::default()),
                LineStatus::Added => ("+", Style::default().fg(Color::Green)),
                LineStatus::Modified => ("~", Style::default().fg(Color::Yellow)),
                LineStatus::Unchanged => (" ", Style::default()),
            };
            let mut gutter_line = vec![Span::styled(ch, style)];
            if num_width > 0 {
                let text = if seg > 0 {
                    String::new()
                } else {
                    let n = match pane_ctx.line_numbers {
                        LineNumbers::Relative if row != pane.cursor_row => {
                            row.abs_diff(pane.cursor_row)
                        }
                        _ => row + 1,
                    };
                    n.to_string()
                };
                let num_style = if row == pane.cursor_row {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                // Right-aligned, with one trailing space before the text.
                gutter_line.push(Span::styled(
                    format!("{text:>w$} ", w = num_width as usize - 1),
                    num_style,
                ));
            }
            gutter_lines.push(Line::from(gutter_line));
        }
        i += run_len;
    }

    frame.render_widget(Paragraph::new(gutter_lines), gutter_area);
    // Full lines; horizontal scroll applies only when not wrapping.
    frame.render_widget(
        Paragraph::new(visible).scroll((0, pane.viewport_left as u16)),
        content_area,
    );

    // Terminal cursor position (for blinking cursor in Insert)
    if is_active {
        let indent = pane.row_indent(pane.cursor_row);
        let (seg, seg_start) = if wrapping {
            let segs = pane.row_segments(pane.cursor_row);
            let seg = segs
                .iter()
                .position(|r| pane.cursor_col < r.end)
                .unwrap_or(segs.len() - 1);
            (seg, segs[seg].start)
        } else {
            (0, pane.viewport_left.saturating_sub(indent))
        };
        if let Some(offset) = layout.iter().position(|&e| e == (pane.cursor_row, seg)) {
            let screen_row = content_area.y + offset as u16;
            let col = if wrapping {
                indent + pane.cursor_col.saturating_sub(seg_start)
            } else {
                (indent + pane.cursor_col).saturating_sub(pane.viewport_left)
            };
            let screen_col = content_area.x + col as u16;
            frame.set_cursor_position((screen_col, screen_row));
        }
    }
}

// ── Line rendering ────────────────────────────────────────────────────────────

/// Group per-cell styles into spans.
fn spans_from(cells: &[(char, Style)]) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut cur_style = Style::default();
    for &(ch, style) in cells {
        if style != cur_style {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), cur_style));
            }
            cur_style = style;
        }
        buf.push(ch);
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, cur_style));
    }
    spans
}

/// The styled display cells of one buffer line (links concealed per `ctx`),
/// with selection, search and cursor overlays applied. Always at least one
/// cell, so empty lines can show the cursor.
fn build_cells(line: &str, row: usize, ctx: &LineCtx<'_>) -> Vec<(char, Style)> {
    let (chars, base) = styled_chars(line, ctx.conceal_links);
    let effective_n = chars.len().max(1); // show at least a cursor cell on empty lines

    let mut cells = Vec::with_capacity(effective_n);
    for i in 0..effective_n {
        let ch = chars.get(i).copied().unwrap_or(' ');
        let mut style = base.get(i).copied().unwrap_or_default();

        // Visual selection overlay
        if ctx.is_active {
            if let Some((sr, sc, er, ec, line_wise)) = ctx.visual_sel {
                if in_selection(row, i, sr, sc, er, ec, line_wise) {
                    style = style.bg(Color::Rgb(80, 80, 130)).fg(Color::White);
                }
            }
        }

        // Search highlight overlay
        if let Some(&(_, _, is_current)) = ctx
            .search_spans
            .iter()
            .find(|&&(start, len, _)| (start..start + len).contains(&i))
        {
            style = if is_current {
                Style::default().bg(Color::Yellow).fg(Color::Black)
            } else {
                Style::default().bg(Color::Rgb(80, 60, 0)).fg(Color::White)
            };
        }

        // Block cursor overlay (Normal / Command / Visual modes)
        if ctx.is_active
            && row == ctx.cursor_row
            && i == ctx.cursor_col
            && !matches!(ctx.mode, Mode::Insert)
        {
            style = Style::default().bg(Color::White).fg(Color::Black);
        }

        cells.push((ch, style));
    }
    cells
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
fn org_styles(chars: &[char]) -> Vec<Style> {
    let n = chars.len();
    let mut styles = vec![Style::default(); n];
    if n == 0 {
        return styles;
    }

    // ── Heading ──────────────────────────────────────────────────────────────
    let star_count = chars.iter().take_while(|&&c| c == '*').count();
    if star_count > 0 && chars.get(star_count) == Some(&' ') {
        style_heading(chars, &mut styles, star_count);
        return styles;
    }

    // ── #+keyword lines ───────────────────────────────────────────────────────
    if chars.starts_with(&['#', '+']) {
        styles.fill(Style::default().fg(Color::DarkGray));
        return styles;
    }

    // ── Drawer / property lines ───────────────────────────────────────────────
    if chars[0] == ':' {
        styles.fill(
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        );
        return styles;
    }

    // ── Inline markup ─────────────────────────────────────────────────────────
    apply_link_styles(chars, &mut styles);
    apply_strikethrough(chars, &mut styles);

    // ── List bullets & checkboxes ─────────────────────────────────────────────
    if let Some(first_non_space) = chars.iter().position(|&c| c != ' ') {
        if matches!(chars[first_non_space], '-' | '+') {
            styles[first_non_space] = Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD);
            // Checkbox: "- [ ] " / "- [X] " / "- [-] "
            let cb_start = first_non_space + 2;
            if chars.get(cb_start) == Some(&'[') && chars.get(cb_start + 2) == Some(&']') {
                let cb_style = match chars.get(cb_start + 1) {
                    Some('X' | 'x') => Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                    Some('-') => Style::default().fg(Color::Yellow),
                    _ => Style::default().fg(Color::DarkGray),
                };
                styles[cb_start..=(cb_start + 2).min(n - 1)].fill(cb_style);
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
    let dim = Style::default().fg(Color::DarkGray);
    let desc_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::UNDERLINED);

    for link in links::find_links(chars, '[', ']') {
        // The visible text: the description, or the URL itself when bare.
        let text = link.desc.clone().unwrap_or_else(|| link.url.clone());
        styles[link.start..text.start].fill(dim);
        styles[text.clone()].fill(desc_style);
        styles[text.end..=link.end].fill(dim);
    }
}

/// Build the display chars/styles for a line with link syntax concealed:
/// `[[url][desc]]` collapses to just `desc`, and `[[url]]` collapses to just
/// `url`. All other styling (headings, strikethrough, etc.) is unaffected.
fn styled_chars(line: &str, conceal: bool) -> (Vec<char>, Vec<Style>) {
    let chars: Vec<char> = line.chars().collect();
    let styles = org_styles(&chars);
    if !conceal {
        return (chars, styles);
    }
    let Some(keep) = wrap::conceal_mask(&chars) else {
        return (chars, styles);
    };

    chars
        .into_iter()
        .zip(styles)
        .zip(keep)
        .filter_map(|(cs, k)| k.then_some(cs))
        .unzip()
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
        let close = (content_start + 1..n).find(|&j| {
            chars[j] == '+'
                && !chars[j - 1].is_whitespace()
                && chars.get(j + 1).is_none_or(|c| !c.is_alphanumeric())
        });
        if let Some(j) = close {
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
        let kw_len = kw.len(); // ASCII, so bytes == chars
        let matches_kw = chars
            .get(pos..pos + kw_len)
            .is_some_and(|w| kw.chars().eq(w.iter().copied()));
        if matches_kw {
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
        let pri_color = match chars[pos + 2] {
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

/// Find the char index of the ':' that opens the trailing tag section
/// (`:foo:bar:` at the end of a heading), or `chars.len()` if there are none.
/// Only colons after `from` (the start of the title) can open the section.
fn find_tags_char_col(chars: &[char], from: usize) -> usize {
    let n = chars.len();
    if chars.last() != Some(&':') {
        return n;
    }
    // `close` is a ':' ending a tag; walk left one tag at a time.
    let mut close = n - 1;
    loop {
        if close == 0 {
            return n;
        }
        let seg_start = chars[..close]
            .iter()
            .rposition(|&c| !is_tag_char(c))
            .map_or(0, |p| p + 1);
        // Empty tag, or the tag starts inside the title/keyword/priority.
        if seg_start == close || seg_start <= from {
            return n;
        }
        // The tag must be preceded by the ':' that separates/opens it.
        if chars[seg_start - 1] != ':' {
            return n;
        }
        let open = seg_start - 1;
        if open == 0 {
            return n;
        }
        // Whitespace before the colon (or the title boundary) opens the section.
        if matches!(chars[open - 1], ' ' | '\t') || open - 1 < from {
            return if open > from { open } else { n };
        }
        close = open;
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
    let (bg, fg) = match app.mode {
        Mode::Normal => (Color::Blue, Color::White),
        Mode::Insert => (Color::Green, Color::Black),
        Mode::Command => (Color::Yellow, Color::Black),
        Mode::Visual { .. } => (Color::Magenta, Color::White),
        Mode::Search | Mode::Browse | Mode::BufferList => (Color::Cyan, Color::Black),
    };
    let mode_style = Style::default().bg(bg).fg(fg).add_modifier(Modifier::BOLD);

    let pane = app.pane();
    let mode_span = Span::styled(format!(" {} ", app.mode.label()), mode_style);

    let file_name = pane
        .file_path
        .as_deref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("[no file]");
    let modified = if pane.modified { " [+]" } else { "" };
    let split_indicator = if app.panes.len() > 1 {
        format!("  [{} panes]", app.panes.len())
    } else {
        String::new()
    };

    let file_span = Span::styled(
        format!(" {file_name}{modified}{split_indicator} "),
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
        _ => app.message.clone().unwrap_or_default(),
    };
    let widget = Paragraph::new(text).style(Style::default().fg(Color::White));
    frame.render_widget(widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn concealed(line: &str) -> String {
        styled_chars(line, true).0.into_iter().collect()
    }

    fn tag_col(line: &str) -> usize {
        let chars: Vec<char> = line.chars().collect();
        find_tags_char_col(&chars, 2)
    }

    #[test]
    fn conceal_links_shows_description_only() {
        assert_eq!(
            concealed("see [[https://example.com][the docs]] here"),
            "see the docs here"
        );
    }

    #[test]
    fn conceal_links_shows_bare_url_when_no_description() {
        assert_eq!(
            concealed("see [[https://example.com]] here"),
            "see https://example.com here"
        );
    }

    #[test]
    fn conceal_links_leaves_plain_text_untouched() {
        assert_eq!(concealed("no links here"), "no links here");
    }

    #[test]
    fn conceal_links_keeps_styles_aligned_with_chars() {
        let (chars, styles) = styled_chars("a [[u][d]] b [[x]]", true);
        assert_eq!(chars.len(), styles.len());
    }

    /// Render `app` into a `width`×`height` test terminal; one string per row,
    /// without the git gutter column.
    fn draw(app: &mut App, width: u16, height: u16) -> (Vec<String>, Option<(u16, u16)>) {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let rows = (0..height)
            .map(|y| {
                (1..width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect();
        let cursor = terminal.get_cursor_position().ok().map(|p| (p.x, p.y));
        (rows, cursor)
    }

    fn app_with(text: &str) -> App {
        let mut app = App::new(None).unwrap();
        app.line_numbers = LineNumbers::Off;
        app.pane_mut().lines = text.lines().map(str::to_owned).collect();
        app
    }

    #[test]
    fn absolute_line_numbers_precede_content() {
        let mut app = app_with("one\ntwo\nthree");
        app.line_numbers = LineNumbers::Absolute;
        let (rows, cursor) = draw(&mut app, 20, 6);
        assert_eq!(rows[0].trim_end(), " 1 one");
        assert_eq!(rows[2].trim_end(), " 3 three");
        // gutter 1 + number column 3
        assert_eq!(cursor.map(|c| c.0), Some(4));
    }

    #[test]
    fn relative_line_numbers_show_distance_and_absolute_on_cursor() {
        let mut app = app_with("a\nb\nc\nd");
        app.line_numbers = LineNumbers::Relative;
        app.pane_mut().cursor_row = 1;
        let (rows, _) = draw(&mut app, 20, 7);
        assert_eq!(rows[0].trim_end(), " 1 a");
        assert_eq!(rows[1].trim_end(), " 2 b");
        assert_eq!(rows[2].trim_end(), " 1 c");
        assert_eq!(rows[3].trim_end(), " 2 d");
    }

    #[test]
    fn line_numbers_blank_on_wrapped_continuation() {
        let mut app = app_with("aaaa bbbb cccc dddd\nnext");
        app.line_numbers = LineNumbers::Absolute;
        // 1 git + 3 number + 10 content
        let (rows, _) = draw(&mut app, 14, 6);
        assert_eq!(rows[0].trim_end(), " 1 aaaa bbbb");
        assert_eq!(rows[1].trim_end(), "   cccc dddd");
        assert_eq!(rows[2].trim_end(), " 2 next");
    }

    #[test]
    fn line_numbers_cycle() {
        assert_eq!(LineNumbers::Off.next(), LineNumbers::Absolute);
        assert_eq!(LineNumbers::Absolute.next(), LineNumbers::Relative);
        assert_eq!(LineNumbers::Relative.next(), LineNumbers::Off);
    }

    #[test]
    fn long_lines_wrap_at_word_boundaries() {
        let mut app = app_with("aaaa bbbb cccc dddd\nnext");
        // 11 columns = 1 gutter + 10 content.
        let (rows, _) = draw(&mut app, 11, 6);
        assert_eq!(rows[0].trim_end(), "aaaa bbbb");
        assert_eq!(rows[1].trim_end(), "cccc dddd");
        assert_eq!(rows[2].trim_end(), "next");
    }

    #[test]
    fn wrap_off_scrolls_horizontally_instead() {
        let mut app = app_with("aaaa bbbb cccc dddd\nnext");
        app.wrap = false;
        let (rows, _) = draw(&mut app, 11, 6);
        assert_eq!(rows[0].trim_end(), "aaaa bbbb");
        assert_eq!(rows[1].trim_end(), "next");
    }

    #[test]
    fn folded_heading_hides_body_and_shows_marker() {
        let mut app = app_with("* A\nbody\n* B");
        app.pane_mut().cycle_fold();
        let (rows, _) = draw(&mut app, 20, 6);
        assert!(rows[0].contains("* A […]"), "{:?}", rows[0]);
        assert_eq!(rows[1].trim_end(), "* B");
    }

    #[test]
    fn cursor_lands_on_wrapped_continuation_row() {
        let mut app = app_with("aaaa bbbb cccc dddd");
        app.mode = Mode::Insert;
        app.pane_mut().cursor_col = 12;
        let (_, cursor) = draw(&mut app, 11, 6);
        // col 12 is the 3rd char of the second segment ("cccc dddd" starts at 10).
        assert_eq!(cursor, Some((3, 1)));
    }

    #[test]
    fn body_under_heading_is_indented_and_wraps_narrower() {
        let mut app = app_with("* H\naaaa bbbb cccc");
        // 1 gutter + 10 content; body indent 2 leaves 8 cells per row.
        let (rows, _) = draw(&mut app, 11, 6);
        assert_eq!(rows[0].trim_end(), "* H");
        assert_eq!(rows[1].trim_end(), "  aaaa");
        assert_eq!(rows[2].trim_end(), "  bbbb");
        assert_eq!(rows[3].trim_end(), "  cccc");
    }

    #[test]
    fn indent_toggle_off_renders_flush_left() {
        let mut app = app_with("* H\nbody");
        app.indent = false;
        let (rows, _) = draw(&mut app, 20, 4);
        assert_eq!(rows[1].trim_end(), "body");
    }

    #[test]
    fn cursor_is_shifted_by_indent() {
        let mut app = app_with("* H\nbody");
        app.pane_mut().cursor_row = 1;
        app.pane_mut().cursor_col = 1;
        let (_, cursor) = draw(&mut app, 20, 4);
        // gutter 1 + indent 2 + col 1
        assert_eq!(cursor, Some((4, 1)));
    }

    #[test]
    fn indent_applies_without_wrap() {
        let mut app = app_with("* H\nbody");
        app.wrap = false;
        app.pane_mut().cursor_row = 1;
        let (rows, cursor) = draw(&mut app, 20, 4);
        assert_eq!(rows[1].trim_end(), "  body");
        assert_eq!(cursor, Some((3, 1)));
    }

    #[test]
    fn tags_found_at_end_of_heading() {
        assert_eq!(tag_col("* Title :foo:bar:"), 8);
        assert_eq!(tag_col("* Title"), "* Title".len());
        assert_eq!(tag_col("* Title :foo"), "* Title :foo".len());
        assert_eq!(tag_col("* Title foo:bar:"), "* Title foo:bar:".len());
    }
}
