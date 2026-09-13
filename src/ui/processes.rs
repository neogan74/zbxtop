//! Tab 1: zabbix_server fork-role aggregates.

use super::graphs::draw_sparklines;
use super::util::*;
use crate::app::App;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

pub(super) fn draw_processes(f: &mut Frame, app: &App, area: Rect) {
    let host = app.focused();
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    // Left column: aggregates by role + comparison with busy% from the server itself.
    // Key UX insight: a divergence where "busy from stats is high, CPU from ps is low"
    // indicates the process is waiting (lock/DB/IO) rather than computing.
    let header = Row::new(vec![
        Cell::from("Role"),
        Cell::from("Cnt"),
        Cell::from("CPU%"), // from ps (what actually consumes CPU)
        Cell::from("RSS"),
        Cell::from("BusyPs"),  // % of forks not idle right now (from proctitle)
        Cell::from("BusyZbx"), // busy.avg from zabbix.stats — what the server reports itself
        Cell::from("Δ"),       // difference BusyZbx - BusyPs (positive = waiting)
        Cell::from("Sample status"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = host
        .roles
        .iter()
        .map(|r| {
            let busy_ps_pct = if r.count == 0 {
                0.0
            } else {
                100.0 * r.busy as f32 / r.count as f32
            };
            // Stats may name the role slightly differently (with spaces/dashes).
            // A simple exact-match lookup works for most roles; if there is no
            // match, the cell is left empty.
            let busy_zbx = host
                .stats
                .as_ref()
                .and_then(|s| s.process.get(&r.role))
                .map(|p| p.busy.avg);
            let delta = busy_zbx.map(|b| b - busy_ps_pct);

            Row::new(vec![
                Cell::from(r.role.clone()),
                Cell::from(r.count.to_string()),
                Cell::from(format!("{:>5.1}", r.cpu_sum))
                    .style(Style::default().fg(pct_color(r.cpu_sum))),
                Cell::from(humanize_kb(r.rss_sum_kb)),
                Cell::from(format!("{:>4.0}%", busy_ps_pct))
                    .style(Style::default().fg(pct_color(busy_ps_pct))),
                Cell::from(
                    busy_zbx
                        .map(|b| format!("{:>4.0}%", b))
                        .unwrap_or_else(|| "  — ".into()),
                )
                .style(
                    busy_zbx
                        .map(|b| Style::default().fg(pct_color(b)))
                        .unwrap_or_default(),
                ),
                Cell::from(
                    delta
                        .map(|d| format!("{:>+4.0}", d))
                        .unwrap_or_else(|| " —".into()),
                )
                .style(delta.map(delta_color).unwrap_or_default()),
                Cell::from(short_one_line(&r.sample_status, 40))
                    .style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(22),
        Constraint::Length(4),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(5),
        Constraint::Min(20),
    ];

    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Roles  (ps vs zabbix.stats) "),
    );
    f.render_widget(table, layout[0]);

    // Right column: sparkline charts
    draw_sparklines(f, app, layout[1]);
}
