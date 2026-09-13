//! Tab 3: streaming log view with level highlighting and filter.

use super::util::*;
use crate::app::App;
use crate::collectors::LogLevel;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

pub(super) fn draw_logs(f: &mut Frame, app: &App, area: Rect) {
    let host = app.focused();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    // Filter field
    let filter_title = if app.editing_filter {
        " Filter (Esc — clear, Enter — apply) "
    } else {
        " Filter (press / to edit) "
    };
    let filter_text = if app.log_filter.is_empty() {
        "(empty)".to_string()
    } else {
        app.log_filter.clone()
    };
    let filter_para = Paragraph::new(filter_text).block(
        Block::default()
            .borders(Borders::ALL)
            .title(filter_title)
            .border_style(if app.editing_filter {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            }),
    );
    f.render_widget(filter_para, layout[0]);

    let filter_lc = app.log_filter.to_lowercase();
    let items: Vec<ListItem> = host
        .logs
        .iter()
        .filter(|l| filter_lc.is_empty() || l.raw.to_lowercase().contains(&filter_lc))
        .rev()
        .take(layout[1].height as usize + 5)
        .map(|l| {
            let color = match l.level {
                LogLevel::Error => Color::Red,
                LogLevel::Warning => Color::Yellow,
                LogLevel::Info => Color::Gray,
                LogLevel::Debug => Color::DarkGray,
                LogLevel::Other => Color::DarkGray,
            };
            ListItem::new(Line::from(Span::styled(
                short_one_line(&l.raw, 1000),
                Style::default().fg(color),
            )))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", host.log_path)),
    );
    f.render_widget(list, layout[1]);
}
