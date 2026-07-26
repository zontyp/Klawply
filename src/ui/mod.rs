//! Rendering. Green phosphor on black, with a seven-segment title. Three
//! screens: connect, resume, chat.

mod seven_segment;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, LineKind, Screen};
use crate::theme;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    // Paint the whole background black first.
    frame.render_widget(Block::default().style(theme::base()), area);

    match app.screen {
        Screen::Connect => draw_connect(frame, app, area),
        Screen::Resume => draw_resume(frame, app, area),
        Screen::Chat => draw_chat(frame, app, area),
    }
}

// ---------------------------------------------------------------------------
// Connect screen
// ---------------------------------------------------------------------------

fn draw_connect(frame: &mut Frame, app: &App, area: Rect) {
    let [title_area, body_area] =
        Layout::vertical([Constraint::Length(6), Constraint::Min(0)]).areas(area);

    // Seven-segment brand title.
    frame.render_widget(seven_segment_title("KLAWPLY"), title_area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "CONNECT CHATGPT SUBSCRIPTION",
        theme::bright(),
    )));
    lines.push(Line::raw(""));

    if !app.email.is_empty() {
        let plan = subscription_label(&app.plan)
            .map(|s| format!(" · {s}"))
            .unwrap_or_default();
        lines.push(Line::from(Span::styled(
            format!("Connected as {}{plan}", app.email),
            theme::base(),
        )));
    } else if let Some(url) = &app.connect_url {
        lines.push(Line::from(Span::styled(
            "Your browser was opened to authorize klawply.",
            theme::base(),
        )));
        lines.push(Line::from(Span::styled(
            "If it didn't open, copy this link (press c) and log in with your ChatGPT account:",
            theme::base(),
        )));
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(url.clone(), theme::accent())));
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "[ press  c  to copy this link to your clipboard ]",
            theme::bright(),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "Log in with your ChatGPT subscription to power klawply.",
            theme::base(),
        )));
    }

    lines.push(Line::raw(""));
    if app.busy {
        for l in loader_lines("") {
            lines.push(l);
        }
        lines.push(Line::raw(""));
    }
    if !app.status.is_empty() {
        lines.push(Line::from(Span::styled(app.status.clone(), theme::dim())));
    }
    lines.push(Line::raw(""));
    let footer = if app.connect_url.is_some() {
        "c: copy link   ·   Enter: retry   ·   q: quit"
    } else {
        "Enter: connect   ·   q: quit"
    };
    lines.push(Line::from(Span::styled(footer, theme::dim())));

    let para = Paragraph::new(Text::from(lines))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: false })
        .style(theme::base());
    frame.render_widget(para, centered(body_area, 80));
}

// ---------------------------------------------------------------------------
// Resume screen
// ---------------------------------------------------------------------------

fn draw_resume(frame: &mut Frame, app: &App, area: Rect) {
    let loader_h: u16 = if app.busy { 3 } else { 0 };
    let [header, loader_area, body, activity, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(loader_h),
        Constraint::Min(3),
        Constraint::Length(6),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("KLAWPLY ", theme::bright()),
            Span::styled("· paste your resume", theme::dim()),
        ]))
        .style(theme::base()),
        header,
    );

    if app.busy {
        frame.render_widget(
            Paragraph::new(Text::from(loader_lines(&app.status))).style(theme::base()),
            loader_area,
        );
    }

    let editor = Block::default()
        .borders(Borders::ALL)
        .border_style(if app.busy { theme::dim() } else { theme::base() })
        .title(Span::styled(" resume.txt ", theme::base()));
    let text = if app.input.is_empty() {
        Text::from(Span::styled("Paste your full resume text here…", theme::dim()))
    } else {
        Text::from(format!("{}\u{2588}", app.input))
    };
    frame.render_widget(
        Paragraph::new(text)
            .block(editor)
            .wrap(Wrap { trim: false })
            .style(theme::base()),
        body,
    );

    // Activity box — the tail of the transcript so status/errors from the
    // extraction step are visible on this screen (not hidden until Chat).
    let activity_block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim())
        .title(Span::styled(" activity ", theme::dim()));
    let mut notes = tail_lines(app, 4);
    if notes.is_empty() {
        notes.push(Line::from(Span::styled(
            "Paste your resume above, then press Enter (or Ctrl+D) to save.",
            theme::dim(),
        )));
    }
    frame.render_widget(
        Paragraph::new(Text::from(notes))
            .block(activity_block)
            .wrap(Wrap { trim: false })
            .style(theme::base()),
        activity,
    );

    frame.render_widget(
        Paragraph::new(Span::styled(
            "Enter / Ctrl+D: save & continue   ·   Ctrl+C: quit",
            theme::dim(),
        ))
        .style(theme::base()),
        footer,
    );
}

/// The last `n` transcript entries as styled lines (for the resume activity box).
fn tail_lines(app: &App, n: usize) -> Vec<Line<'static>> {
    let start = app.transcript.len().saturating_sub(n);
    let mut out = Vec::new();
    for entry in &app.transcript[start..] {
        let style = match entry.kind {
            LineKind::Error => theme::error(),
            LineKind::System => theme::base(),
            _ => theme::dim(),
        };
        for raw in entry.text.split('\n') {
            out.push(Line::from(Span::styled(raw.to_string(), style)));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Chat screen
// ---------------------------------------------------------------------------

fn draw_chat(frame: &mut Frame, app: &mut App, area: Rect) {
    let loader_h: u16 = if app.busy { 3 } else { 0 };
    let [header, loader_area, transcript_area, input_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(loader_h),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .areas(area);

    // Header: brand + who you're signed in as.
    let mut header_spans = vec![Span::styled("KLAWPLY", theme::bright())];
    if !app.email.is_empty() {
        header_spans.push(Span::styled(format!("   {}", app.email), theme::dim()));
    }
    if let Some(sub) = subscription_label(&app.plan) {
        header_spans.push(Span::styled(format!("   {sub}"), theme::dim()));
    }
    frame.render_widget(
        Paragraph::new(Line::from(header_spans)).style(theme::base()),
        header,
    );

    // Loading animation with the current status.
    if app.busy {
        frame.render_widget(
            Paragraph::new(Text::from(loader_lines(&app.status))).style(theme::base()),
            loader_area,
        );
    }

    // Transcript. We pre-wrap into concrete visual lines (rather than let
    // Paragraph wrap) so mouse cells map exactly back to text for selection.
    let width = transcript_area.width.max(1) as usize;
    let wrapped = wrap_transcript(app, width);
    let total = wrapped.len();
    let height = transcript_area.height as usize;
    let max_scroll = total.saturating_sub(height);
    if (app.scroll as usize) > max_scroll {
        app.scroll = max_scroll as u16;
    }
    let top = max_scroll.saturating_sub(app.scroll as usize);

    // Publish the wrapped lines + area so the app can resolve mouse selection.
    app.set_chat_view(
        wrapped.iter().map(|(t, _)| t.clone()).collect(),
        transcript_area,
        top,
    );

    let range = app.selection_range();
    let visible: Vec<Line> = (top..(top + height).min(total))
        .map(|idx| {
            let (text, style) = &wrapped[idx];
            highlight_line(idx, text, *style, range)
        })
        .collect();
    frame.render_widget(
        Paragraph::new(Text::from(visible)).style(theme::base()),
        transcript_area,
    );

    // Input box. Shows a placeholder until the user types.
    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_style(if app.busy { theme::dim() } else { theme::base() });
    let input_line = if app.input.is_empty() && !app.busy {
        Line::from(vec![
            Span::styled("› ", theme::accent()),
            Span::styled("Paste a job link to apply, or ask a question…", theme::dim()),
        ])
    } else {
        let cursor = if app.busy { "" } else { "\u{2588}" };
        Line::from(vec![
            Span::styled("› ", theme::accent()),
            Span::styled(format!("{}{cursor}", app.input), theme::bright()),
        ])
    };
    frame.render_widget(
        Paragraph::new(input_line)
            .block(input_block)
            .wrap(Wrap { trim: false })
            .style(theme::base()),
        input_area,
    );
}

/// Flatten the transcript into concrete wrapped visual lines paired with their
/// style. Each speaker prefix is folded into the text so what you select is
/// exactly what you see (and copy).
fn wrap_transcript(app: &App, width: usize) -> Vec<(String, Style)> {
    let mut out: Vec<(String, Style)> = Vec::new();
    for entry in &app.transcript {
        let (prefix, style) = match entry.kind {
            LineKind::User => ("you › ", theme::bright()),
            LineKind::Assistant => ("gpt › ", theme::base()),
            LineKind::System => ("· ", theme::dim()),
            LineKind::Error => ("✗ ", theme::error()),
        };
        for (i, raw) in entry.text.split('\n').enumerate() {
            let logical = if i == 0 {
                format!("{prefix}{raw}")
            } else {
                format!("  {raw}")
            };
            for visual in wrap_line(&logical, width) {
                out.push((visual, style));
            }
        }
    }
    out
}

/// Turn a raw plan claim ("plus", "pro", …) into a friendly subscription label,
/// e.g. "ChatGPT Plus subscription". Empty plan → no label.
fn subscription_label(plan: &str) -> Option<String> {
    let plan = plan.trim();
    if plan.is_empty() {
        return None;
    }
    let mut chars = plan.chars();
    let capitalized = chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default();
    Some(format!("ChatGPT {capitalized} subscription"))
}

/// Greedy word-wrap to `width` columns, hard-breaking words longer than the
/// width. Always yields at least one (possibly empty) line.
fn wrap_line(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for word in text.split(' ') {
        let wlen = word.chars().count();
        if wlen > width {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let mut chunk = String::new();
            let mut clen = 0;
            for ch in word.chars() {
                if clen == width {
                    lines.push(std::mem::take(&mut chunk));
                    clen = 0;
                }
                chunk.push(ch);
                clen += 1;
            }
            cur = chunk;
            cur_len = clen;
            continue;
        }
        let projected = if cur.is_empty() { wlen } else { cur_len + 1 + wlen };
        if projected > width {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
            cur_len = wlen;
        } else {
            if !cur.is_empty() {
                cur.push(' ');
                cur_len += 1;
            }
            cur.push_str(word);
            cur_len += wlen;
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Build a rendered line, reverse-video highlighting the selected columns.
fn highlight_line(
    idx: usize,
    text: &str,
    style: Style,
    range: Option<((usize, usize), (usize, usize))>,
) -> Line<'static> {
    let Some((start, end)) = range.filter(|(s, e)| idx >= s.0 && idx <= e.0) else {
        return Line::from(Span::styled(text.to_string(), style));
    };
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let (from, to) = if start.0 == end.0 {
        (start.1, end.1)
    } else if idx == start.0 {
        (start.1, len)
    } else if idx == end.0 {
        (0, end.1)
    } else {
        (0, len)
    };
    let from = from.min(len);
    let to = to.min(len);

    let take = |a: usize, b: usize| -> String { chars[a..b].iter().collect() };
    let mut spans: Vec<Span> = Vec::new();
    if from > 0 {
        spans.push(Span::styled(take(0, from), style));
    }
    if from < to {
        spans.push(Span::styled(
            take(from, to),
            style.add_modifier(Modifier::REVERSED),
        ));
    }
    if to < len {
        spans.push(Span::styled(take(to, len), style));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), style));
    }
    Line::from(spans)
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Render `text` in the seven-segment font as a centered green title widget.
fn seven_segment_title(text: &str) -> Paragraph<'static> {
    let rows = seven_segment::render(text);
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|row| Line::from(Span::styled(row, theme::bright())))
        .collect();
    Paragraph::new(Text::from(lines))
        .alignment(Alignment::Center)
        .style(theme::base())
}

/// A horizontally centered sub-rect `percent` wide.
fn centered(area: Rect, percent: u16) -> Rect {
    let [_, mid, _] = Layout::horizontal([
        Constraint::Percentage((100 - percent) / 2),
        Constraint::Percentage(percent),
        Constraint::Percentage((100 - percent) / 2),
    ])
    .areas(area);
    mid
}

// ---------------------------------------------------------------------------
// Loading animation: a small black square with a green dot orbiting its border.
// ---------------------------------------------------------------------------

/// The twelve perimeter cells of a 3-row × 5-col square, clockwise from the
/// top-left corner. The dot advances one cell per animation step.
const PERIMETER: [(usize, usize); 12] = [
    (0, 0), (0, 1), (0, 2), (0, 3), (0, 4), // top edge, left→right
    (1, 4), (2, 4),                         // right edge, top→bottom
    (2, 3), (2, 2), (2, 1), (2, 0),         // bottom edge, right→left
    (1, 0),                                 // left edge, bottom→top
];

/// Current orbit step, keyed off wall-clock time so it advances on every
/// redraw. The event loop ticks redraws ~11 fps while busy (see main.rs).
fn loader_step() -> usize {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    (millis / 90) as usize
}

/// The static border glyph for cell `(r, c)` of the 3×5 square.
fn square_border(r: usize, c: usize) -> char {
    match (r, c) {
        (0, 0) => '┌',
        (0, 4) => '┐',
        (2, 0) => '└',
        (2, 4) => '┘',
        (0, _) | (2, _) => '─',
        (_, 0) | (_, 4) => '│',
        _ => ' ',
    }
}

/// Render the square as three styled lines with the green dot at the current
/// orbit position. If `label` is non-empty it is shown to the right of the
/// square's middle row.
fn loader_lines(label: &str) -> Vec<Line<'static>> {
    let (dot_r, dot_c) = PERIMETER[loader_step() % PERIMETER.len()];
    let mut lines: Vec<Line> = Vec::with_capacity(3);
    for r in 0..3 {
        let mut spans: Vec<Span> = Vec::with_capacity(5);
        for c in 0..5 {
            if r == dot_r && c == dot_c {
                spans.push(Span::styled("●", theme::bright()));
            } else {
                spans.push(Span::styled(square_border(r, c).to_string(), theme::dim()));
            }
        }
        if r == 1 && !label.is_empty() {
            spans.push(Span::styled(format!("  {label}"), theme::accent()));
        }
        lines.push(Line::from(spans));
    }
    lines
}
