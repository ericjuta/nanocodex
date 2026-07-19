use ratatui::{
    Frame,
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use super::app::{App, ToolStatus, TranscriptItem};

pub(super) fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    let composer_height = composer_height(&app.input, area.width.saturating_sub(4));
    let [header_area, transcript_area, composer_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, app, header_area);
    render_transcript(frame, app, transcript_area);
    render_composer(frame, app, composer_area);
    render_footer(frame, app, footer_area);
}

fn render_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let title = Line::from(vec![
        Span::styled(
            " nanocodex ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            app.cwd.display().to_string(),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(title), area);
}

fn render_transcript(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let text = transcript_text(app);
    let line_count = wrapped_line_count(&text, inner.width);
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    let max_scroll = line_count.saturating_sub(usize::from(inner.height));
    let scroll = max_scroll.saturating_sub(app.scroll_from_bottom.min(max_scroll));
    frame.render_widget(paragraph.scroll((saturating_u16(scroll), 0)), inner);
}

fn render_composer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let title = if app.running {
        " Message (Enter queues) "
    } else {
        " Message "
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.running {
            Color::Yellow
        } else {
            Color::Cyan
        }));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (cursor_row, cursor_column) = composer_cursor(app, inner.width.max(1));
    let vertical_scroll = cursor_row.saturating_sub(inner.height.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(app.input.as_str())
            .wrap(Wrap { trim: false })
            .scroll((vertical_scroll, 0)),
        inner,
    );

    let x = inner
        .x
        .saturating_add(cursor_column.min(inner.width.saturating_sub(1)));
    let y = inner
        .y
        .saturating_add(cursor_row.saturating_sub(vertical_scroll));
    frame.set_cursor_position(Position::new(x, y));
}

fn render_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let state = if app.running {
        format!("{} {}", spinner[app.frame % spinner.len()], app.status)
    } else {
        app.status.clone()
    };
    let queued = app.pending_turns.saturating_sub(usize::from(app.running));
    let queue = if queued == 0 {
        String::new()
    } else {
        format!("  ·  {queued} queued")
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {state}{queue}"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                "    Enter send  Ctrl+J newline  PgUp/PgDn scroll  Ctrl+C quit",
                Style::default().fg(Color::DarkGray),
            ),
        ])),
        area,
    );
}

fn transcript_text(app: &App) -> Text<'static> {
    if app.transcript.is_empty() {
        return Text::from(vec![
            Line::raw(""),
            Line::styled(
                "  Ask Nanocodex to inspect, edit, run, or explain this workspace.",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
    }

    let mut lines = Vec::new();
    for item in &app.transcript {
        match item {
            TranscriptItem::User(message) => {
                lines.push(Line::styled(
                    "› You",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ));
                push_body(&mut lines, message, Style::default().fg(Color::White));
            }
            TranscriptItem::Assistant(message) => {
                lines.push(Line::styled(
                    "● Nanocodex",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ));
                push_body(&mut lines, message, Style::default().fg(Color::White));
            }
            TranscriptItem::Tool {
                name,
                arguments,
                status,
                ..
            } => {
                let (icon, color) = match status {
                    ToolStatus::Running => ("◌", Color::Yellow),
                    ToolStatus::Completed => ("✓", Color::Green),
                    ToolStatus::Failed => ("✗", Color::Red),
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{icon} {name}"), Style::default().fg(color)),
                    Span::styled(
                        format!("  {arguments}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
            TranscriptItem::Error(message) => {
                lines.push(Line::styled(
                    format!("✗ {message}"),
                    Style::default().fg(Color::Red),
                ));
            }
        }
        lines.push(Line::raw(""));
    }
    Text::from(lines)
}

fn push_body(lines: &mut Vec<Line<'static>>, body: &str, style: Style) {
    for line in body.split('\n') {
        lines.push(Line::styled(format!("  {line}"), style));
    }
}

fn composer_height(input: &str, width: u16) -> u16 {
    let width = usize::from(width.max(1));
    let rows = input
        .split('\n')
        .map(|line| UnicodeWidthStr::width(line).div_ceil(width).max(1))
        .sum::<usize>();
    saturating_u16(rows).clamp(1, 7).saturating_add(2)
}

fn composer_cursor(app: &App, width: u16) -> (u16, u16) {
    let width = usize::from(width.max(1));
    let before = &app.input[..app.cursor];
    let mut row = 0_usize;
    let mut lines = before.split('\n').peekable();
    while let Some(line) = lines.next() {
        let columns = UnicodeWidthStr::width(line);
        if lines.peek().is_some() {
            row = row.saturating_add(columns / width + 1);
        } else {
            row = row.saturating_add(columns / width);
            return (saturating_u16(row), saturating_u16(columns % width));
        }
    }
    (0, 0)
}

fn saturating_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

fn wrapped_line_count(text: &Text<'_>, width: u16) -> usize {
    let width = usize::from(width.max(1));
    text.lines
        .iter()
        .map(|line| line.width().div_ceil(width).max(1))
        .sum()
}
