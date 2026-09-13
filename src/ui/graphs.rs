//! Tab 2: gauges and ASCII sparklines.

use crate::app::App;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Gauge, Sparkline},
    Frame,
};

pub(super) fn draw_graphs(f: &mut Frame, app: &App, area: Rect) {
    let host = app.focused();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(area);

    draw_sparkline(
        f,
        chunks[0],
        " CPU (sum of zabbix_server forks, %) ",
        &host.history.cpu_total,
        100.0,
        Color::Cyan,
    );
    draw_sparkline(
        f,
        chunks[1],
        " Memory used (%) ",
        &host.history.mem_used_pct,
        100.0,
        Color::Green,
    );
    draw_sparkline(
        f,
        chunks[2],
        " Load average (1m) ",
        &host.history.load1,
        8.0,
        Color::Yellow,
    );
}

pub(super) fn draw_sparklines(f: &mut Frame, app: &App, area: Rect) {
    let host = app.focused();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(4),
            Constraint::Length(4),
            Constraint::Min(0),
        ])
        .split(area);

    draw_sparkline(
        f,
        chunks[0],
        " CPU sum % ",
        &host.history.cpu_total,
        100.0,
        Color::Cyan,
    );
    draw_sparkline(
        f,
        chunks[1],
        " Mem % ",
        &host.history.mem_used_pct,
        100.0,
        Color::Green,
    );
    draw_sparkline(
        f,
        chunks[2],
        " Load 1m ",
        &host.history.load1,
        8.0,
        Color::Yellow,
    );

    // Bottom — instant-value gauges
    let g_area = chunks[3];
    let gauges = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(g_area);

    let cpu = (host.cpu_sum().clamp(0.0, 100.0) / 100.0 * 100.0) as u16;
    let mem = host.sys.mem_used_pct().clamp(0.0, 100.0) as u16;
    let swap = host.sys.swap_used_pct().clamp(0.0, 100.0) as u16;

    f.render_widget(gauge(" CPU sum ", cpu, Color::Cyan), gauges[0]);
    f.render_widget(gauge(" Mem    ", mem, Color::Green), gauges[1]);
    f.render_widget(gauge(" Swap   ", swap, Color::Magenta), gauges[2]);
}

pub(super) fn gauge<'a>(title: &'a str, pct: u16, color: Color) -> Gauge<'a> {
    Gauge::default()
        .block(Block::default().title(title))
        .gauge_style(Style::default().fg(color))
        .percent(pct.min(100))
}

pub(super) fn draw_sparkline(
    f: &mut Frame,
    area: Rect,
    title: &str,
    data: &std::collections::VecDeque<f64>,
    _max: f64,
    color: Color,
) {
    // Ratatui Sparkline wants u64, so we scale (×100 for percentages and load average
    // gives enough dynamic range).
    let values: Vec<u64> = data.iter().map(|v| (v * 100.0).max(0.0) as u64).collect();
    let last_str = data
        .back()
        .map(|v| format!("{:.2}", v))
        .unwrap_or_else(|| "—".into());
    let sp = Sparkline::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("{title} last={last_str}")),
        )
        .data(&values)
        .style(Style::default().fg(color));
    f.render_widget(sp, area);
}
