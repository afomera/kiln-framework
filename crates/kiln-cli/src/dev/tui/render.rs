//! Drawing the TUI: header, process sidebar, output pane, key hints.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect, Size},
    style::{Color, Modifier, Style},
    text::{Line as TextLine, Span},
    widgets::{Block, Borders, Paragraph},
};

use super::{PromptKind, Ui, View, ansi};
use crate::dev::{
    lines::Line,
    supervisor::{Kind, Proc, Status, Supervisor},
};

/// Smaller than this, the TUI only says so.
pub const MIN_COLS: u16 = 60;
pub const MIN_ROWS: u16 = 10;

const COLORS: [Color; 6] = [Color::Cyan, Color::Magenta, Color::Yellow, Color::Green, Color::Blue, Color::Red];
const ACCENT: Color = Color::Indexed(208);

/// The color for process `id`'s name.
fn color(procs: &[Proc], id: usize) -> Color {
    match procs.get(id).map(|p| &p.kind) {
        Some(Kind::Service { .. }) | None => COLORS[id % COLORS.len()],
        Some(_) => Color::Gray,
    }
}

fn sidebar_width(procs: &[Proc]) -> u16 {
    let name = procs.iter().map(|p| p.name.len()).max().unwrap_or(5) as u16;
    (name + 21).clamp(24, 36)
}

/// The output pane's text area (columns, rows) for a terminal of `cols` x `rows`.
pub fn pane_size(cols: u16, rows: u16, procs: &[Proc]) -> (u16, u16) {
    let cols = cols.saturating_sub(sidebar_width(procs)).saturating_sub(1).max(20);
    (cols, rows.saturating_sub(3).max(5))
}

/// [`pane_size`] for the current terminal size and view.
pub fn pane_size_for(ui: &Ui, procs: &[Proc], size: Option<Size>) -> (u16, u16) {
    match size {
        Some(size) => pane_size(size.width, size.height, procs),
        None => ui.pane,
    }
}

fn spinner() -> &'static str {
    let tick = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_millis() / 120;
    ["◐", "◓", "◑", "◒"][(tick % 4) as usize]
}

/// The status dot and its style.
fn dot(status: Status) -> (&'static str, Style) {
    let style = Style::default();
    match status {
        Status::Waiting | Status::Stopped => ("○", style.fg(Color::DarkGray)),
        Status::Running => ("●", style.fg(Color::Green)),
        Status::Compiling | Status::Building => (spinner(), style.fg(Color::Yellow)),
        Status::CompileFailed | Status::BuildFailed | Status::Failed => ("✗", style.fg(Color::Red)),
        Status::Built => ("✓", style.fg(Color::Green)),
        Status::Restarting => ("↻", style.fg(Color::Yellow)),
        Status::Stopping => ("◌", style.fg(Color::Yellow)),
        Status::Exited => ("✓", style.fg(Color::DarkGray)),
        Status::Crashed => ("●", style.fg(Color::Red)),
        Status::Listed => ("≡", style.fg(Color::Cyan)),
    }
}

/// Draws the whole screen.
pub fn draw(frame: &mut Frame, ui: &mut Ui, sup: &Supervisor) {
    let area = frame.area();
    if area.width < MIN_COLS || area.height < MIN_ROWS {
        let message =
            format!("Terminal too small ({}x{}); need {MIN_COLS}x{MIN_ROWS}. q quits.", area.width, area.height);
        frame.render_widget(Paragraph::new(message).style(Style::default().fg(Color::Yellow)), area);
        return;
    }
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let [sidebar, main] =
        Layout::horizontal([Constraint::Length(sidebar_width(&sup.procs)), Constraint::Fill(1)]).areas(body);
    let [title, content] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(main);
    draw_header(frame, header, sup);
    draw_sidebar(frame, sidebar, ui, sup);
    draw_title(frame, title, ui, sup);
    draw_output(frame, content, ui, sup);
    draw_footer(frame, footer, ui);
}

fn draw_header(frame: &mut Frame, area: Rect, sup: &Supervisor) {
    let dim = Style::default().fg(Color::DarkGray);
    let mut spans = vec![
        Span::styled(" kiln dev ", Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(sup.url(), Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED)),
        Span::styled("  o opens /kiln", dim),
    ];
    let build = sup.procs[sup.build].status();
    let (symbol, style) = dot(build);
    let message = match build {
        Status::Building => "building".to_owned(),
        Status::BuildFailed if sup.built() => "build failed · running the last good build".to_owned(),
        Status::BuildFailed => "build failed · waiting for changes".to_owned(),
        _ => String::new(),
    };
    if !message.is_empty() {
        spans.extend([Span::raw("   "), Span::styled(format!("{symbol} {message}"), style)]);
    }
    if sup.quitting() {
        spans.extend([Span::raw("   "), Span::styled("stopping…", Style::default().fg(Color::Yellow))]);
    }
    frame.render_widget(Paragraph::new(TextLine::from(spans)), area);
}

fn draw_sidebar(frame: &mut Frame, area: Rect, ui: &Ui, sup: &Supervisor) {
    let block = Block::new().borders(Borders::RIGHT).border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let name_width = sup.procs.iter().map(|p| p.name.len()).max().unwrap_or(5);
    let mut rows = vec![];
    for (id, proc) in sup.procs.iter().enumerate() {
        let status = proc.status();
        let (symbol, style) = dot(status);
        let key = if id < 9 { format!(" {} ", id + 1) } else { "   ".to_owned() };
        let selected = id == ui.selected && ui.view == View::Tabs;
        let mut row = TextLine::from(vec![
            Span::styled(key, Style::default().fg(Color::DarkGray)),
            Span::styled(symbol, style),
            Span::raw(" "),
            Span::styled(
                format!("{:<name_width$}", proc.name),
                Style::default().fg(color(&sup.procs, id)).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", status.label()), Style::default().fg(Color::DarkGray)),
        ]);
        if selected {
            row = row.style(Style::default().bg(Color::Indexed(236)));
        }
        if id == sup.build && id > 0 {
            rows.push(TextLine::from(""));
        }
        rows.push(row);
    }
    if ui.view == View::Stream {
        rows.push(TextLine::from(""));
        rows.push(TextLine::from(Span::styled(" m: back to tabs", Style::default().fg(Color::DarkGray))));
    }
    frame.render_widget(Paragraph::new(rows), inner);
}

fn draw_title(frame: &mut Frame, area: Rect, ui: &Ui, sup: &Supervisor) {
    let dim = Style::default().fg(Color::DarkGray);
    let mut spans = match ui.view {
        View::Tabs => {
            let proc = &sup.procs[ui.selected];
            let (symbol, style) = dot(proc.status());
            let mut spans = vec![
                Span::styled(
                    format!(" {} ", proc.name),
                    Style::default().fg(color(&sup.procs, ui.selected)).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {symbol} {}", proc.status().label()), style),
            ];
            if let Some(reason) = proc.failure() {
                spans.push(Span::styled(format!(": {reason}"), style));
            }
            if let Some(pid) = proc.pid() {
                spans.push(Span::styled(format!("  pid {pid}"), dim));
            }
            spans.push(Span::styled(format!("  {}", proc.command()), dim));
            spans
        }
        View::Stream => vec![Span::styled(" all processes ", Style::default().add_modifier(Modifier::BOLD))],
    };
    if let Some(filter) = &ui.filter {
        spans.push(Span::styled(format!("  filter: {filter}"), Style::default().fg(Color::Yellow)));
    }
    if let Some(search) = &ui.search {
        spans.push(Span::styled(format!("  search: {search}"), Style::default().fg(Color::Yellow)));
    }
    frame.render_widget(Paragraph::new(TextLine::from(spans)).style(Style::default().bg(Color::Indexed(235))), area);
}

/// `line` as rows at most `width` wide, with `search` matches highlighted.
fn rows(line: &Line, width: usize, search: Option<&str>) -> Vec<Vec<Span<'static>>> {
    let base = if line.system {
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)
    } else {
        Style::default()
    };
    let runs = ansi::parse(&line.text, base);
    let marks = search.map(|q| ansi::find(&line.plain, q)).unwrap_or_default();
    let mut runs = ansi::highlight(runs, &marks, Style::default().fg(Color::Black).bg(Color::Yellow));
    if line.system {
        runs.insert(0, (base, "▸ ".into()));
    }
    ansi::wrap(&runs, width)
}

fn draw_output(frame: &mut Frame, area: Rect, ui: &mut Ui, sup: &Supervisor) {
    let [text_area, bar_area] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let (width, height) = (text_area.width as usize, text_area.height as usize);
    ui.height = height;
    let lines = ui.lines(sup);
    let label_width = sup.procs.iter().map(|p| p.name.len()).max().unwrap_or(3);
    let stream = ui.view == View::Stream;
    let search = ui.search.clone();
    let partial = match ui.view {
        View::Tabs if ui.filter.is_none() => sup.procs[ui.selected].partial(),
        _ => None,
    };
    let ui_routes = ui.view == View::Tabs && sup.procs[ui.selected].kind == Kind::Routes;
    let scroll = match ui.view {
        View::Stream => &mut ui.stream,
        View::Tabs => &mut ui.scrolls[ui.selected],
    };
    let len = lines.len();
    // The routes list is a table: it opens at its first row, not following the end.
    if ui_routes && !scroll.opened && len > 0 {
        scroll.offset = len;
    }
    scroll.opened |= len > 0;
    scroll.offset = scroll.offset.min(len.saturating_sub(1));
    if scroll.offset == 0 {
        scroll.unseen = false;
    }

    let make = |index: usize| -> Vec<TextLine<'static>> {
        let (id, line) = lines[index];
        if !stream {
            let rows = rows(line, width, search.as_deref()).into_iter().map(TextLine::from);
            // A table: rows too wide for the pane are cut, not wrapped.
            return if sup.procs[id].kind == Kind::Routes { rows.take(1).collect() } else { rows.collect() };
        }
        let label = Span::styled(
            format!("{:>label_width$} │ ", sup.procs[id].name),
            Style::default().fg(color(&sup.procs, id)),
        );
        let indent = Span::raw(" ".repeat(label_width + 3));
        rows(line, width.saturating_sub(label_width + 3), search.as_deref())
            .into_iter()
            .enumerate()
            .map(|(i, mut row)| {
                row.insert(0, if i == 0 { label.clone() } else { indent.clone() });
                TextLine::from(row)
            })
            .collect()
    };

    let mut shown: std::collections::VecDeque<TextLine<'static>> = Default::default();
    if scroll.offset == 0
        && let Some(partial) = &partial
    {
        shown.extend(rows(partial, width, search.as_deref()).into_iter().map(TextLine::from));
    }
    let bottom = len.saturating_sub(1).saturating_sub(scroll.offset);
    let mut bottom_drawn = bottom;
    if len > 0 {
        let mut index = bottom as isize;
        while shown.len() < height && index >= 0 {
            for row in make(index as usize).into_iter().rev() {
                shown.push_front(row);
            }
            index -= 1;
        }
        while shown.len() > height {
            shown.pop_front();
        }
        // At the top with room to spare: fill downward, so `g` shows the first page.
        let mut next = bottom + 1;
        while shown.len() < height && next < len {
            let rows = make(next);
            if shown.len() + rows.len() > height {
                break;
            }
            shown.extend(rows);
            bottom_drawn = next;
            next += 1;
        }
    }
    scroll.bottom = bottom_drawn;

    if len == 0 && shown.is_empty() {
        let message = if stream {
            "No output yet."
        } else {
            match sup.procs[ui.selected].status() {
                Status::Waiting => "Waiting for the first build.",
                _ => "No output yet.",
            }
        };
        shown.push_back(TextLine::from(Span::styled(message, Style::default().fg(Color::DarkGray))));
    }
    frame.render_widget(Paragraph::new(Vec::from(shown)), text_area);

    // Scrollbar: the thumb's size and place show which part of the buffer is visible.
    if len > height && height > 0 {
        let thumb = (height * height / len).max(1);
        let top = bottom_drawn.saturating_sub(height.saturating_sub(1));
        let start = (top * (height - thumb)) / len.saturating_sub(height).max(1);
        let start = start.min(height - thumb);
        let bar: Vec<TextLine> = (0..height)
            .map(|row| {
                if row >= start && row < start + thumb {
                    TextLine::from(Span::styled("┃", Style::default().fg(Color::Gray)))
                } else {
                    TextLine::from(Span::styled("│", Style::default().fg(Color::Indexed(238))))
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(bar), bar_area);
    }

    if scroll.unseen && scroll.offset > 0 {
        let marker = " new output ↓  G ";
        let w = marker.chars().count() as u16;
        if text_area.width > w + 2 && text_area.height > 0 {
            let at = Rect { x: text_area.right() - w - 1, y: text_area.bottom() - 1, width: w, height: 1 };
            frame.render_widget(Paragraph::new(marker).style(Style::default().fg(Color::Black).bg(Color::Yellow)), at);
        }
    }
}

fn draw_footer(frame: &mut Frame, area: Rect, ui: &Ui) {
    let key = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);
    if let Some((kind, text)) = &ui.prompt {
        let label = match kind {
            PromptKind::Search => "/",
            PromptKind::Filter => "filter: ",
        };
        let line = TextLine::from(vec![
            Span::styled(format!(" {label}"), key),
            Span::raw(text.clone()),
            Span::styled("▏", Style::default().fg(ACCENT)),
            Span::styled("   Enter to apply · Esc to cancel", dim),
        ]);
        return frame.render_widget(Paragraph::new(line), area);
    }
    if let Some((message, at)) = &ui.flash
        && at.elapsed() < Duration::from_secs(3)
    {
        return frame.render_widget(Paragraph::new(format!(" {message}")).style(Style::default().fg(ACCENT)), area);
    }
    let view = if ui.view == View::Tabs { "stream" } else { "tabs" };
    let hints = [
        ("q", "quit"),
        ("1-9", "select"),
        ("j/k", "scroll"),
        ("g/G", "top/end"),
        ("r", "restart"),
        ("s", "start/stop"),
        ("/", "search"),
        ("n/N", "next"),
        ("f", "filter"),
        ("m", view),
        ("c", "clear"),
        ("o", "open"),
        ("K", "check"),
        ("T", "test"),
    ];
    let mut spans = vec![Span::raw(" ")];
    let mut used = 1;
    for (k, label) in hints {
        let w = k.chars().count() + label.len() + 3;
        if used + w > area.width as usize {
            break;
        }
        spans.extend([Span::styled(k, key), Span::styled(format!(" {label}  "), dim)]);
        used += w;
    }
    frame.render_widget(Paragraph::new(TextLine::from(spans)), area);
}
