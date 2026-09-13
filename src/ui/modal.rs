//! Modals: runtime-control menu and host picker (Ctrl-G).

use super::util::*;
use crate::app::App;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

pub(super) fn draw_runtime_modal(f: &mut Frame, app: &App) {
    let host = app.focused();
    let area = centered_rect(50, 40, f.area());
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = app
        .runtime_choices
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let marker = if i == app.runtime_cursor {
                "▶ "
            } else {
                "  "
            };
            let style = if i == app.runtime_cursor {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(
                format!("{marker}{} (zabbix_server -R {})", c.label(), c.arg()),
                style,
            )))
        })
        .collect();

    let title = if host.use_sudo {
        " Runtime control  [sudo -n]  ↑↓ select  Enter run  Esc close "
    } else {
        " Runtime control  ↑↓ select  Enter run  Esc close "
    };
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(list, area);
}

/// v0.5c — fuzzy host-picker overlay. Top is the query field, below is a
/// ranked list of hosts with a cursor marker. Useful with 20+ hosts where
/// cycling through Ctrl-N/P takes too long.
pub(super) fn draw_host_picker(f: &mut Frame, app: &App) {
    let area = centered_rect(60, 60, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Host picker  ↑↓ select  Enter switch  Esc close ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Layout: query (3) + counter (1) + list (rest).
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inner);

    // Query field.
    let query_para = Paragraph::new(Line::from(vec![
        Span::raw("> "),
        Span::styled(
            app.host_picker_query.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("█", Style::default().fg(Color::Yellow)),
    ]))
    .block(Block::default().borders(Borders::BOTTOM));
    f.render_widget(query_para, layout[0]);

    let matches = app.host_picker_matches();
    let counter = Paragraph::new(Line::from(Span::styled(
        format!("{}/{} hosts match", matches.len(), app.hosts.len()),
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(counter, layout[1]);

    // Match list.
    let items: Vec<ListItem> = matches
        .iter()
        .enumerate()
        .map(|(row, &host_idx)| {
            let h = &app.hosts[host_idx];
            let is_cursor = row == app.host_picker_cursor;
            let is_focused = host_idx == app.focused_host;
            let marker = match (is_cursor, is_focused) {
                (true, true) => "▸●",
                (true, false) => "▸ ",
                (false, true) => " ●",
                (false, false) => "  ",
            };
            let row_style = if is_cursor {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{marker} "), row_style),
                Span::styled(h.name.clone(), row_style.add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!("  {}", h.ssh_host),
                    Style::default().fg(if is_cursor {
                        Color::Black
                    } else {
                        Color::DarkGray
                    }),
                ),
            ]))
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, layout[2]);
}

// ---------- utils ----------
