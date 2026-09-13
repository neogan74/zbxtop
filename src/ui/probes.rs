//! Tab 6: synthetic probe results (v0.7).

use super::util::*;
use crate::app::App;
use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap},
    Frame,
};

/// v0.7 — synthetic probes. Global (not tied to the focused host).
pub(super) fn draw_probes(f: &mut Frame, app: &App, area: Rect) {
    if app.probes.is_empty() {
        let hint = "No probes configured.\n\
                    Add `[[probe]]` blocks to ~/.config/ztop/hosts.toml:\n\n\
                    [[probe]]\n\
                    name = \"DB primary\"\n\
                    kind = \"tcp\"\n\
                    target = \"db-prod-01:5432\"\n\n\
                    Supported kinds: tcp, dns, pg.";
        let p = Paragraph::new(hint)
            .block(Block::default().borders(Borders::ALL).title(" Probes "))
            .wrap(Wrap { trim: false });
        f.render_widget(p, area);
        return;
    }

    let header = Row::new(vec![
        Cell::from("Name"),
        Cell::from("Kind"),
        Cell::from("Target"),
        Cell::from("Latency"),
        Cell::from("Status"),
        Cell::from("Success"),
        Cell::from("Last error"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app
        .probes
        .iter()
        .map(|p| {
            let lat_str = p
                .last_latency
                .map(|d| format!("{:>5.1}ms", d.as_secs_f64() * 1000.0))
                .unwrap_or_else(|| "  —".into());
            let lat_color = match p.last_latency {
                Some(d) if d.as_millis() > 1000 => Color::Red,
                Some(d) if d.as_millis() > 100 => Color::Yellow,
                Some(_) => Color::Green,
                None => Color::DarkGray,
            };
            let status_str: &str = if p.consecutive_errors > 0 {
                "ERR"
            } else if p.last_ok.is_some() {
                "OK"
            } else {
                "—"
            };
            let status_color = if p.consecutive_errors > 0 {
                Color::Red
            } else if p.last_ok.is_some() {
                Color::Green
            } else {
                Color::DarkGray
            };
            let pct_str = p
                .success_pct()
                .map(|x| format!("{:>5.1}%", x))
                .unwrap_or_else(|| "  —".into());
            let pct_color_v = p
                .success_pct()
                .map(|x| {
                    if x >= 99.0 {
                        Color::Green
                    } else if x >= 90.0 {
                        Color::Yellow
                    } else {
                        Color::Red
                    }
                })
                .unwrap_or(Color::DarkGray);
            let err_str = p
                .last_error
                .as_deref()
                .map(|e| short_one_line(e, 80))
                .unwrap_or_default();

            Row::new(vec![
                Cell::from(p.name.clone()).style(Style::default().add_modifier(Modifier::BOLD)),
                Cell::from(p.kind.clone()).style(Style::default().fg(Color::Cyan)),
                Cell::from(short_one_line(&p.target_display, 40))
                    .style(Style::default().fg(Color::DarkGray)),
                Cell::from(lat_str).style(Style::default().fg(lat_color)),
                Cell::from(status_str).style(Style::default().fg(status_color)),
                Cell::from(pct_str).style(Style::default().fg(pct_color_v)),
                Cell::from(err_str).style(Style::default().fg(Color::Red)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(20),
        Constraint::Length(6),
        Constraint::Length(40),
        Constraint::Length(9),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Min(20),
    ];
    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Probes ({}) ", app.probes.len())),
    );
    f.render_widget(table, area);
}
