//! Shared helpers: layout, color scales, humanized units.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
};

pub(super) fn centered_rect(pct_x: u16, pct_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Color of the Δ column: red — the server is heavily "busy" but ps does not
/// see it (typical sign of waiting on lock/DB/IO). Green — busy and CPU agree.
pub(super) fn delta_color(d: f32) -> Style {
    if d >= 30.0 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if d >= 10.0 {
        Style::default().fg(Color::Yellow)
    } else if d <= -10.0 {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::Green)
    }
}

pub(super) fn pct_color(p: f32) -> Color {
    if p >= 85.0 {
        Color::Red
    } else if p >= 60.0 {
        Color::Yellow
    } else if p > 0.0 {
        Color::Green
    } else {
        Color::DarkGray
    }
}

pub(super) fn load_color(l: f32) -> Color {
    // Approximate threshold — without a CPU count one can't be more precise, but fine for a prototype.
    if l >= 8.0 {
        Color::Red
    } else if l >= 4.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

pub(super) fn humanize_kb(kb: u64) -> String {
    let mb = kb as f64 / 1024.0;
    if mb < 1024.0 {
        format!("{:.0}M", mb)
    } else {
        format!("{:.1}G", mb / 1024.0)
    }
}

pub(super) fn humanize_uptime(sec: u64) -> String {
    let d = sec / 86400;
    let h = (sec % 86400) / 3600;
    let m = (sec % 3600) / 60;
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m}m")
    } else {
        format!("{m}m")
    }
}

pub(super) fn short_one_line(s: &str, max: usize) -> String {
    let one = s.replace('\n', " ").replace('\r', "");
    if one.chars().count() > max {
        one.chars().take(max).collect::<String>() + "…"
    } else {
        one
    }
}
