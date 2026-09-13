//! Tab 0: fleet overview table and aggregate.

use super::util::*;
use crate::app::App;
use crate::diagnose::{Diagnosis, Severity};
use crate::source::SourceKind;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};
use std::time::Duration;

/// v0.5a/b — overview for all hosts. Top — per-host table;
/// bottom — aggregate row across the entire fleet (sum CPU, max load, sum queue,
/// total active diagnoses).
pub(super) fn draw_overview(f: &mut Frame, app: &App, diagnoses: &[Vec<Diagnosis>], area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(4)])
        .split(area);

    draw_overview_table(f, app, diagnoses, chunks[0]);
    draw_overview_aggregate(f, app, diagnoses, chunks[1]);
}

pub(super) fn draw_overview_table(
    f: &mut Frame,
    app: &App,
    diagnoses: &[Vec<Diagnosis>],
    area: Rect,
) {
    let header = Row::new(vec![
        Cell::from(""),
        Cell::from("Host"),
        Cell::from("SSH"),
        Cell::from("CPU%"),
        Cell::from("Mem%"),
        Cell::from("Load1"),
        Cell::from("Queue"),
        Cell::from("Diag"),
        Cell::from("Sources"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app
        .hosts
        .iter()
        .enumerate()
        .map(|(i, h)| {
            // Traffic light: healthy source count vs total source count.
            let total = if h.stats_enabled { 4 } else { 3 } + if h.db_enabled { 1 } else { 0 };
            let healthy = [
                SourceKind::Procs,
                SourceKind::Sys,
                SourceKind::Logs,
                SourceKind::Stats,
                SourceKind::Db,
            ]
            .iter()
            .filter(|k| {
                let enabled = match k {
                    SourceKind::Stats => h.stats_enabled,
                    SourceKind::Db => h.db_enabled,
                    _ => true,
                };
                enabled
                    && h.source(**k).last_ok.is_some()
                    && !h.source(**k).is_stale(Duration::from_secs(15))
            })
            .count();
            let (sym, sym_color) = if healthy == total {
                ("🟢", Color::Green)
            } else if healthy == 0 {
                ("🔴", Color::Red)
            } else {
                ("🟡", Color::Yellow)
            };

            let cpu_sum = h.cpu_sum();
            let mem = h.sys.mem_used_pct();
            let load = h.sys.load1;
            let queue = h
                .stats
                .as_ref()
                .and_then(|s| s.queue_total())
                .map(|q| q.to_string())
                .unwrap_or_else(|| "—".into());
            let diag_count = diagnoses[i].len();

            // Two markers: cursor (for selection) and focus (currently open host).
            // `▸` — where the cursor is now, `●` — which host is open in drill-down.
            let marker = match (i == app.overview_cursor, i == app.focused_host) {
                (true, true) => "▸●",
                (true, false) => "▸ ",
                (false, true) => " ●",
                (false, false) => "  ",
            };
            let row_style = if i == app.overview_cursor {
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::Rgb(40, 40, 50))
            } else if i == app.focused_host {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(format!("{marker} {sym}")).style(Style::default().fg(sym_color)),
                Cell::from(h.name.clone()).style(row_style),
                Cell::from(h.ssh_host.clone()).style(Style::default().fg(Color::DarkGray)),
                Cell::from(format!("{:>5.1}", cpu_sum))
                    .style(Style::default().fg(pct_color(cpu_sum as f32))),
                Cell::from(format!("{:>4.0}%", mem)).style(Style::default().fg(pct_color(mem))),
                Cell::from(format!("{:>5.2}", load)).style(Style::default().fg(load_color(load))),
                Cell::from(queue),
                {
                    let has_crit = diagnoses[i]
                        .iter()
                        .any(|d| d.severity == Severity::Critical);
                    let (label, color) = if diag_count == 0 {
                        ("✓".into(), Color::Green)
                    } else if has_crit {
                        (format!("✖ {diag_count}"), Color::Red)
                    } else {
                        (format!("⚠ {diag_count}"), Color::Yellow)
                    };
                    Cell::from(label).style(Style::default().fg(color).add_modifier(Modifier::BOLD))
                },
                Cell::from(format!("{}/{}", healthy, total))
                    .style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(5),
        Constraint::Length(20),
        Constraint::Length(28),
        Constraint::Length(7),
        Constraint::Length(6),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(5),
        Constraint::Length(9),
    ];
    let table = Table::new(rows, widths).header(header).block(
        Block::default().borders(Borders::ALL).title(format!(
            " Hosts  ({}) — Ctrl-N/P switch focus ",
            app.hosts.len()
        )),
    );
    f.render_widget(table, area);
}

/// Fleet-level summary below the Overview table. Shows how the entire server
/// pool is doing in a single line.
pub(super) fn draw_overview_aggregate(
    f: &mut Frame,
    app: &App,
    diagnoses: &[Vec<Diagnosis>],
    area: Rect,
) {
    let n = app.hosts.len() as f64;
    let total_cpu: f64 = app.hosts.iter().map(|h| h.cpu_sum()).sum();
    let total_queue: u64 = app
        .hosts
        .iter()
        .filter_map(|h| h.stats.as_ref().and_then(|s| s.queue_total()))
        .sum();
    let max_load: f32 = app
        .hosts
        .iter()
        .map(|h| h.sys.load1)
        .fold(0.0_f32, f32::max);
    let max_mem: f32 = app
        .hosts
        .iter()
        .map(|h| h.sys.mem_used_pct())
        .fold(0.0_f32, f32::max);
    let avg_mem = if n > 0.0 {
        app.hosts
            .iter()
            .map(|h| h.sys.mem_used_pct() as f64)
            .sum::<f64>()
            / n
    } else {
        0.0
    };
    let total_diag: usize = diagnoses.iter().map(|ds| ds.len()).sum();
    let critical_diag: usize = diagnoses
        .iter()
        .flat_map(|ds| ds.iter())
        .filter(|d| d.severity == Severity::Critical)
        .count();

    let healthy = app
        .hosts
        .iter()
        .filter(|h| {
            let stale = h
                .source(SourceKind::Procs)
                .is_stale(Duration::from_secs(15));
            !stale
        })
        .count();

    let line1 = Line::from(vec![
        Span::raw("Fleet "),
        Span::styled(
            format!("{}/{}", healthy, app.hosts.len()),
            Style::default()
                .fg(if healthy == app.hosts.len() {
                    Color::Green
                } else {
                    Color::Yellow
                })
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" healthy   "),
        Span::raw("Σ CPU "),
        Span::styled(
            format!("{:.0}%", total_cpu),
            Style::default().fg(pct_color((total_cpu / n.max(1.0)) as f32)),
        ),
        Span::raw("   max load "),
        Span::styled(
            format!("{:.2}", max_load),
            Style::default().fg(load_color(max_load)),
        ),
        Span::raw("   mem avg/max "),
        Span::styled(
            format!("{:.0}%/{:.0}%", avg_mem, max_mem),
            Style::default().fg(pct_color(max_mem)),
        ),
    ]);
    let line2 = Line::from(vec![
        Span::raw("Σ queue "),
        Span::styled(
            total_queue.to_string(),
            Style::default().fg(if total_queue > 5000 {
                Color::Red
            } else if total_queue > 500 {
                Color::Yellow
            } else {
                Color::Green
            }),
        ),
        Span::raw("   diagnoses "),
        Span::styled(
            total_diag.to_string(),
            Style::default()
                .fg(if critical_diag > 0 {
                    Color::Red
                } else if total_diag > 0 {
                    Color::Yellow
                } else {
                    Color::Green
                })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if critical_diag > 0 {
                format!("  ({} critical)", critical_diag)
            } else if total_diag > 0 {
                "  (warnings only)".to_string()
            } else {
                "  (all clear)".to_string()
            },
            Style::default().fg(if critical_diag > 0 {
                Color::Red
            } else {
                Color::DarkGray
            }),
        ),
    ]);

    let para = Paragraph::new(vec![line1, line2]).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Fleet aggregate "),
    );
    f.render_widget(para, area);
}
