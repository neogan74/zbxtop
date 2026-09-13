//! Tab 5: DB scraper view (connections, queries, locks, sizes, replication).

use super::util::*;
use crate::app::App;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap},
    Frame,
};

pub(super) fn draw_database(f: &mut Frame, app: &App, area: Rect) {
    let host = app.focused();
    let Some(db) = host.db.as_ref() else {
        let hint = if host.db_enabled {
            "Waiting for first DB response…\n\
             Verify --db-url, read-only role permissions, network reachability."
        } else {
            "DB source disabled. Set --db-url postgres://user:pass@host:5432/zabbix to enable."
        };
        let p = Paragraph::new(hint)
            .block(Block::default().borders(Borders::ALL).title(" Database "))
            .wrap(Wrap { trim: true });
        f.render_widget(p, area);
        return;
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),  // summary line
            Constraint::Min(10),    // top queries
            Constraint::Length(12), // table sizes
        ])
        .split(area);

    // ---- summary ----
    let conn = &db.connections;
    let lag = match db.replication_lag_sec {
        None => "primary".to_string(),
        Some(s) => format!("replica lag {:.1}s", s),
    };
    let lag_color = match db.replication_lag_sec {
        None => Color::Green,
        Some(s) if s < 1.0 => Color::Green,
        Some(s) if s < 10.0 => Color::Yellow,
        _ => Color::Red,
    };
    let backend_label = match db.backend.as_str() {
        "mysql" => "MySQL",
        "postgres" => "PG",
        other if !other.is_empty() => other,
        _ => "DB",
    };
    let summary = Paragraph::new(vec![
        Line::from(vec![
            Span::raw(format!("{backend_label} ")),
            Span::styled(db.version.clone(), Style::default().fg(Color::Yellow)),
            Span::raw("   "),
            Span::styled(lag, Style::default().fg(lag_color)),
            Span::raw("   locks waiting "),
            Span::styled(
                db.locks_waiting.to_string(),
                Style::default().fg(if db.locks_waiting > 5 {
                    Color::Red
                } else if db.locks_waiting > 0 {
                    Color::Yellow
                } else {
                    Color::Green
                }),
            ),
        ]),
        Line::from(vec![
            Span::raw("Connections  total "),
            Span::styled(
                conn.total().to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("   active "),
            Span::styled(conn.active.to_string(), Style::default().fg(Color::Green)),
            Span::raw("   idle "),
            Span::styled(conn.idle.to_string(), Style::default().fg(Color::DarkGray)),
            Span::raw("   idle-in-tx "),
            Span::styled(
                conn.idle_in_transaction.to_string(),
                Style::default().fg(if conn.idle_in_transaction > 5 {
                    Color::Red
                } else if conn.idle_in_transaction > 0 {
                    Color::Yellow
                } else {
                    Color::DarkGray
                }),
            ),
            Span::raw("   waiting "),
            Span::styled(
                conn.waiting.to_string(),
                Style::default().fg(if conn.waiting > 0 {
                    Color::Red
                } else {
                    Color::DarkGray
                }),
            ),
        ]),
    ])
    .block(Block::default().borders(Borders::ALL).title(" DB summary "));
    f.render_widget(summary, layout[0]);

    // ---- top queries ----
    let header = Row::new(vec![
        Cell::from("Pid"),
        Cell::from("Age"),
        Cell::from("State"),
        Cell::from("WaitEvent"),
        Cell::from("User"),
        Cell::from("Query"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = db
        .top_queries
        .iter()
        .map(|q| {
            let age_color = if q.age_sec >= 60.0 {
                Color::Red
            } else if q.age_sec >= 10.0 {
                Color::Yellow
            } else {
                Color::Green
            };
            Row::new(vec![
                Cell::from(q.pid.to_string()),
                Cell::from(format!("{:>6.1}s", q.age_sec)).style(Style::default().fg(age_color)),
                Cell::from(q.state.clone()),
                Cell::from(q.wait_event.clone().unwrap_or_default())
                    .style(Style::default().fg(Color::Magenta)),
                Cell::from(q.usename.clone()).style(Style::default().fg(Color::DarkGray)),
                Cell::from(short_one_line(&q.query, 200)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(18),
        Constraint::Length(12),
        Constraint::Min(20),
    ];
    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Top long-running queries "),
    );
    f.render_widget(table, layout[1]);

    // ---- table sizes ----
    let max_bytes = db.tables.iter().map(|t| t.bytes).max().unwrap_or(1).max(1);
    let mut t_rows = Vec::new();
    for t in &db.tables {
        // Простой ASCII-бар по доле от максимальной таблицы.
        let frac = t.bytes as f32 / max_bytes as f32;
        let bar_len = (frac * 30.0) as usize;
        let bar = "█".repeat(bar_len);
        t_rows.push(Row::new(vec![
            Cell::from(t.name.clone()),
            Cell::from(t.pretty.clone()).style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from(bar).style(Style::default().fg(Color::Cyan)),
        ]));
    }
    let widths = [
        Constraint::Length(28),
        Constraint::Length(12),
        Constraint::Min(15),
    ];
    let table = Table::new(t_rows, widths)
        .header(
            Row::new(vec![
                Cell::from("Zabbix table"),
                Cell::from("Size"),
                Cell::from("Relative"),
            ])
            .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Zabbix table sizes "),
        );
    f.render_widget(table, layout[2]);
}
