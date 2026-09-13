//! Tab 4: zabbix.stats internals (queue, vps, busy%, caches).

use super::util::*;
use crate::app::App;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Gauge, Paragraph, Row, Table, Wrap},
    Frame,
};

pub(super) fn draw_internals(f: &mut Frame, app: &App, area: Rect) {
    let host = app.focused();
    let Some(stats) = host.stats.as_ref() else {
        let hint = if host.stats_enabled {
            "Waiting for first zabbix.stats response…\n\
             Make sure StatsAllowedIP in zabbix_server.conf includes this IP\n\
             and that trapper port (default 10051) is reachable."
        } else {
            "Stats source disabled (--no-stats). Restart with stats enabled to use this panel."
        };
        let p = Paragraph::new(hint)
            .block(Block::default().borders(Borders::ALL).title(" Internals "))
            .wrap(Wrap { trim: true });
        f.render_widget(p, area);
        return;
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // server identity
            Constraint::Min(10),   // processes table
            Constraint::Length(6), // caches + queue + vps
        ])
        .split(area);

    // Server identity header
    let ident = Paragraph::new(Line::from(vec![
        Span::raw("Zabbix server "),
        Span::styled(
            format!("{}@{}", stats.version, stats.hostname),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("   uptime "),
        Span::styled(
            humanize_uptime(stats.uptime),
            Style::default().fg(Color::Green),
        ),
        if let Some(q) = stats.queue_total() {
            Span::styled(
                format!("   queue {q}"),
                Style::default().fg(if q > 1000 {
                    Color::Red
                } else if q > 100 {
                    Color::Yellow
                } else {
                    Color::Green
                }),
            )
        } else {
            Span::raw("")
        },
        if let Some(v) = stats.vps_total() {
            Span::styled(format!("   vps {:.0}", v), Style::default().fg(Color::Cyan))
        } else {
            Span::raw("")
        },
    ]))
    .block(Block::default().borders(Borders::ALL).title(" Server "));
    f.render_widget(ident, layout[0]);

    // Process table from stats: busy avg/max/min + count.
    let header = Row::new(vec![
        Cell::from("Process type"),
        Cell::from("Cnt"),
        Cell::from("Busy avg"),
        Cell::from("Busy max"),
        Cell::from("Busy min"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let mut rows: Vec<(String, &crate::zbxstats::ProcessStats)> =
        stats.process.iter().map(|(k, v)| (k.clone(), v)).collect();
    // Sort by busy.avg descending — hottest processes at the top.
    rows.sort_by(|a, b| {
        b.1.busy
            .avg
            .partial_cmp(&a.1.busy.avg)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let body: Vec<Row> = rows
        .iter()
        .map(|(name, p)| {
            Row::new(vec![
                Cell::from(name.clone()),
                Cell::from(p.count.to_string()),
                Cell::from(format!("{:>5.1}%", p.busy.avg))
                    .style(Style::default().fg(pct_color(p.busy.avg))),
                Cell::from(format!("{:>5.1}%", p.busy.max))
                    .style(Style::default().fg(pct_color(p.busy.max))),
                Cell::from(format!("{:>5.1}%", p.busy.min))
                    .style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(28),
        Constraint::Length(4),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
    ];
    let table = Table::new(body, widths).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Processes  (zabbix.stats busy %) "),
    );
    f.render_widget(table, layout[1]);

    // Caches: write/read/value — rendered as gauges.
    let cache_area = layout[2];
    let cache_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(2),
        ])
        .split(cache_area);

    for (i, (label, name)) in [
        ("Write cache  ", crate::zbxstats::CacheName::Write),
        ("Read  cache  ", crate::zbxstats::CacheName::Read),
        ("Value cache  ", crate::zbxstats::CacheName::Value),
    ]
    .iter()
    .enumerate()
    {
        let used = stats.cache_used_pct(*name).unwrap_or(0.0).clamp(0.0, 100.0);
        let g = Gauge::default()
            .block(Block::default().title(format!("{label}({:.0}%)", used)))
            .gauge_style(Style::default().fg(pct_color(used)))
            .percent(used as u16);
        f.render_widget(g, cache_chunks[i]);
    }
}
