//! Ratatui-рендеринг. Один файл — для прототипа сойдёт.

use crate::app::{App, Tab};
use crate::collectors::LogLevel;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Gauge, List, ListItem, Paragraph, Row, Sparkline, Table, Wrap},
    Frame,
};

pub fn draw(f: &mut Frame, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header
            Constraint::Length(1), // tabs
            Constraint::Min(0),    // body
            Constraint::Length(1), // footer
        ])
        .split(f.area());

    draw_header(f, app, root[0]);
    draw_tabs(f, app, root[1]);
    match app.tab {
        Tab::Processes => draw_processes(f, app, root[2]),
        Tab::Graphs => draw_graphs(f, app, root[2]),
        Tab::Logs => draw_logs(f, app, root[2]),
    }
    draw_footer(f, app, root[3]);

    if app.show_runtime_menu {
        draw_runtime_modal(f, app);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let refreshed = app
        .last_refresh
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "—".into());
    let mem_pct = app.sys.mem_used_pct();
    let swap_pct = app.sys.swap_used_pct();
    let uptime = humanize_uptime(app.sys.uptime_sec);
    let pause = if app.paused { " [PAUSED]" } else { "" };

    let line1 = Line::from(vec![
        Span::styled(
            "ztop ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::raw("@ "),
        Span::styled(&app.host, Style::default().fg(Color::Yellow)),
        Span::raw("   load "),
        Span::styled(
            format!("{:.2} {:.2} {:.2}", app.sys.load1, app.sys.load5, app.sys.load15),
            Style::default().fg(load_color(app.sys.load1)),
        ),
        Span::raw("   mem "),
        Span::styled(
            format!("{:.0}%", mem_pct),
            Style::default().fg(pct_color(mem_pct)),
        ),
        Span::raw("   swap "),
        Span::styled(
            format!("{:.0}%", swap_pct),
            Style::default().fg(pct_color(swap_pct)),
        ),
        Span::raw("   uptime "),
        Span::raw(uptime),
        Span::raw("   ts "),
        Span::raw(refreshed),
        Span::styled(pause, Style::default().fg(Color::Magenta)),
    ]);

    let line2 = if let Some(t) = &app.toast {
        let color = if t.is_error { Color::Red } else { Color::Green };
        Line::from(Span::styled(t.msg.clone(), Style::default().fg(color)))
    } else if let Some(e) = &app.last_error {
        Line::from(Span::styled(
            format!("⚠ {}", short_one_line(e, 200)),
            Style::default().fg(Color::Red),
        ))
    } else {
        Line::from(Span::styled(
            format!("procs: {}  roles: {}", app.procs.len(), app.roles.len()),
            Style::default().fg(Color::DarkGray),
        ))
    };

    let para = Paragraph::new(vec![line1, line2]).wrap(Wrap { trim: true });
    f.render_widget(para, area);
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
    let make = |label: &str, active: bool| -> Span<'_> {
        if active {
            Span::styled(
                format!(" {label} "),
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), Style::default().fg(Color::Gray))
        }
    };
    let line = Line::from(vec![
        make("1 Processes", app.tab == Tab::Processes),
        Span::raw(" "),
        make("2 Graphs", app.tab == Tab::Graphs),
        Span::raw(" "),
        make("3 Logs", app.tab == Tab::Logs),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_processes(f: &mut Frame, app: &App, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    // Левая колонка: агрегаты по ролям.
    let header = Row::new(vec![
        Cell::from("Role"),
        Cell::from("Cnt"),
        Cell::from("CPU%"),
        Cell::from("RSS"),
        Cell::from("Busy"),
        Cell::from("Sample status"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app
        .roles
        .iter()
        .map(|r| {
            let busy_pct = if r.count == 0 {
                0
            } else {
                (100 * r.busy / r.count) as u32
            };
            Row::new(vec![
                Cell::from(r.role.clone()),
                Cell::from(r.count.to_string()),
                Cell::from(format!("{:>5.1}", r.cpu_sum)).style(Style::default().fg(pct_color(r.cpu_sum))),
                Cell::from(humanize_kb(r.rss_sum_kb)),
                Cell::from(format!("{busy_pct}%")).style(Style::default().fg(pct_color(busy_pct as f32))),
                Cell::from(short_one_line(&r.sample_status, 60))
                    .style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(22),
        Constraint::Length(4),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(6),
        Constraint::Min(20),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(" Roles "));
    f.render_widget(table, layout[0]);

    // Правая колонка: мини-графики
    draw_sparklines(f, app, layout[1]);
}

fn draw_graphs(f: &mut Frame, app: &App, area: Rect) {
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
        &app.history.cpu_total,
        100.0,
        Color::Cyan,
    );
    draw_sparkline(
        f,
        chunks[1],
        " Memory used (%) ",
        &app.history.mem_used_pct,
        100.0,
        Color::Green,
    );
    draw_sparkline(
        f,
        chunks[2],
        " Load average (1m) ",
        &app.history.load1,
        8.0,
        Color::Yellow,
    );
}

fn draw_sparklines(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(4),
            Constraint::Length(4),
            Constraint::Min(0),
        ])
        .split(area);

    draw_sparkline(f, chunks[0], " CPU sum % ", &app.history.cpu_total, 100.0, Color::Cyan);
    draw_sparkline(f, chunks[1], " Mem % ", &app.history.mem_used_pct, 100.0, Color::Green);
    draw_sparkline(f, chunks[2], " Load 1m ", &app.history.load1, 8.0, Color::Yellow);

    // Внизу — гейджи мгновенных значений
    let g_area = chunks[3];
    let gauges = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)])
        .split(g_area);

    let cpu = (app.cpu_sum().min(100.0).max(0.0) / 100.0 * 100.0) as u16;
    let mem = app.sys.mem_used_pct().clamp(0.0, 100.0) as u16;
    let swap = app.sys.swap_used_pct().clamp(0.0, 100.0) as u16;

    f.render_widget(gauge(" CPU sum ", cpu, Color::Cyan), gauges[0]);
    f.render_widget(gauge(" Mem    ", mem, Color::Green), gauges[1]);
    f.render_widget(gauge(" Swap   ", swap, Color::Magenta), gauges[2]);
}

fn gauge<'a>(title: &'a str, pct: u16, color: Color) -> Gauge<'a> {
    Gauge::default()
        .block(Block::default().title(title))
        .gauge_style(Style::default().fg(color))
        .percent(pct.min(100))
}

fn draw_sparkline(
    f: &mut Frame,
    area: Rect,
    title: &str,
    data: &std::collections::VecDeque<f64>,
    _max: f64,
    color: Color,
) {
    // Sparkline ratatui хочет u64, поэтому масштабируем (×100 для процентов и loadavg
    // даёт достаточно динамики).
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

fn draw_logs(f: &mut Frame, app: &App, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    // Поле фильтра
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
    let items: Vec<ListItem> = app
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
            .title(format!(" {} ", app.log_path)),
    );
    f.render_widget(list, layout[1]);
}

fn draw_footer(f: &mut Frame, _app: &App, area: Rect) {
    let line = Line::from(vec![
        Span::styled(" [Tab] ", Style::default().fg(Color::Cyan)),
        Span::raw("switch  "),
        Span::styled("[r] ", Style::default().fg(Color::Cyan)),
        Span::raw("refresh  "),
        Span::styled("[p] ", Style::default().fg(Color::Cyan)),
        Span::raw("pause  "),
        Span::styled("[/] ", Style::default().fg(Color::Cyan)),
        Span::raw("filter  "),
        Span::styled("[+/-] ", Style::default().fg(Color::Cyan)),
        Span::raw("log level  "),
        Span::styled("[c] ", Style::default().fg(Color::Cyan)),
        Span::raw("cfg reload  "),
        Span::styled("[h] ", Style::default().fg(Color::Cyan)),
        Span::raw("housekeeper  "),
        Span::styled("[d] ", Style::default().fg(Color::Cyan)),
        Span::raw("diaginfo  "),
        Span::styled("[R] ", Style::default().fg(Color::Cyan)),
        Span::raw("menu  "),
        Span::styled("[q] ", Style::default().fg(Color::Cyan)),
        Span::raw("quit"),
    ]);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(Color::Reset)),
        area,
    );
}

fn draw_runtime_modal(f: &mut Frame, app: &App) {
    let area = centered_rect(50, 40, f.area());
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = app
        .runtime_choices
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let marker = if i == app.runtime_cursor { "▶ " } else { "  " };
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

    let title = if app.use_sudo {
        " Runtime control  [sudo -n]  ↑↓ select  Enter run  Esc close "
    } else {
        " Runtime control  ↑↓ select  Enter run  Esc close "
    };
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(list, area);
}

// ---------- utils ----------

fn centered_rect(pct_x: u16, pct_y: u16, r: Rect) -> Rect {
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

fn pct_color(p: f32) -> Color {
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

fn load_color(l: f32) -> Color {
    // Условный порог — без числа CPU точнее не скажешь, но для прототипа сойдёт.
    if l >= 8.0 {
        Color::Red
    } else if l >= 4.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn humanize_kb(kb: u64) -> String {
    let mb = kb as f64 / 1024.0;
    if mb < 1024.0 {
        format!("{:.0}M", mb)
    } else {
        format!("{:.1}G", mb / 1024.0)
    }
}

fn humanize_uptime(sec: u64) -> String {
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

fn short_one_line(s: &str, max: usize) -> String {
    let one = s.replace('\n', " ").replace('\r', "");
    if one.chars().count() > max {
        one.chars().take(max).collect::<String>() + "…"
    } else {
        one
    }
}
