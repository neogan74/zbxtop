//! Ratatui-рендеринг. Один файл — для прототипа сойдёт.

use crate::app::{App, Tab};
use crate::collectors::LogLevel;
use crate::diagnose::{diagnose, Severity};
use crate::source::{SourceKind, SourceState};
use std::time::Duration;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Cell, Chart, Clear, Dataset, Gauge, GraphType, List, ListItem, Paragraph, Row, Table, Wrap},
    Frame,
};

pub fn draw(f: &mut Frame, app: &App) {
    // Cross-source диагнозы по всем хостам, с префиксом [name]. Показываем
    // максимум 3 — самые severe. Высота строки динамическая.
    let mut diagnoses: Vec<_> = app
        .hosts
        .iter()
        .flat_map(|h| {
            diagnose(h).into_iter().map(move |mut d| {
                d.title = format!("[{}] {}", h.name, d.title);
                d
            })
        })
        .collect();
    diagnoses.sort_by(|a, b| b.severity.cmp(&a.severity));
    diagnoses.truncate(3);
    let diag_height = diagnoses.len() as u16;

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),           // header
            Constraint::Length(1),           // tabs
            Constraint::Length(diag_height), // diagnoses, 0-3 строк
            Constraint::Min(0),              // body
            Constraint::Length(1),           // footer
        ])
        .split(f.area());

    draw_header(f, app, root[0]);
    draw_tabs(f, app, root[1]);
    if diag_height > 0 {
        draw_diagnoses(f, &diagnoses, root[2]);
    }
    match app.tab {
        Tab::Overview => draw_overview(f, app, root[3]),
        Tab::Processes => draw_processes(f, app, root[3]),
        Tab::Graphs => draw_graphs(f, app, root[3]),
        Tab::Logs => draw_logs(f, app, root[3]),
        Tab::Internals => draw_internals(f, app, root[3]),
        Tab::Database => draw_database(f, app, root[3]),
        Tab::Probes => draw_probes(f, app, root[3]),
    }
    draw_footer(f, app, root[4]);

    if app.show_runtime_menu {
        draw_runtime_modal(f, app);
    }
    // v0.5c: host-picker рисуется поверх всего, включая runtime modal,
    // чтобы Ctrl-G работал даже при открытом runtime control.
    if app.show_host_picker {
        draw_host_picker(f, app);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let host = app.focused();
    let refreshed = app
        .last_refresh
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "—".into());
    let mem_pct = host.sys.mem_used_pct();
    let swap_pct = host.sys.swap_used_pct();
    let uptime = humanize_uptime(host.sys.uptime_sec);
    let pause = if app.paused { " [PAUSED]" } else { "" };
    let mode_badge: Option<(String, Color)> = if app.is_replay {
        let pct = app.replay_progress.load(std::sync::atomic::Ordering::Relaxed) as f32 / 10.0;
        let replay_paused = app.replay_paused_flag.load(std::sync::atomic::Ordering::Relaxed);
        let marker = if replay_paused { "⏸ " } else { "▶ " };
        let pause_tag = if replay_paused { " [PAUSED]" } else { "" };
        Some((
            format!("{marker}REPLAY {:.1}%{pause_tag}", pct),
            Color::Blue,
        ))
    } else if app.recorded_events > 0 {
        Some((
            format!("● REC ({} ev)", app.recorded_events),
            Color::Red,
        ))
    } else {
        None
    };

    let line1 = Line::from(vec![
        Span::styled(
            "ztop ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::raw("@ "),
        Span::styled(&host.name, Style::default().fg(Color::Yellow)),
        Span::raw("   load "),
        Span::styled(
            format!("{:.2} {:.2} {:.2}", host.sys.load1, host.sys.load5, host.sys.load15),
            Style::default().fg(load_color(host.sys.load1)),
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
        match &mode_badge {
            Some((text, color)) => Span::styled(
                format!("   {}", text),
                Style::default().fg(*color).add_modifier(Modifier::BOLD),
            ),
            None => Span::raw(""),
        },
    ]);

    // Вторая строка шапки: статус каждого источника.
    let mut line2_spans = Vec::with_capacity(16);

    // Procs и Sys — periodic-источники: бейдж по SourceState (age + duration).
    for kind in [SourceKind::Procs, SourceKind::Sys] {
        let s = host.source(kind);
        let (sym, color) = source_badge(s);
        line2_spans.push(Span::raw(" "));
        line2_spans.push(Span::styled(
            format!("[{} {}]", kind.label(), sym),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        line2_spans.push(Span::raw(" "));
        line2_spans.push(Span::styled(
            source_summary(s),
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Logs — streaming: бейдж по log_stream.connected, а не по age (молчание
    // лога — норма; авария — когда стрим оборвался).
    let (sym, color, summary) = log_badge(app);
    line2_spans.push(Span::raw(" "));
    line2_spans.push(Span::styled(
        format!("[log {}]", sym),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    line2_spans.push(Span::raw(" "));
    line2_spans.push(Span::styled(summary, Style::default().fg(Color::DarkGray)));

    // Stats — periodic; показываем только если transport включён.
    if host.stats_enabled {
        let s = host.source(SourceKind::Stats);
        let (sym, color) = source_badge(s);
        line2_spans.push(Span::raw(" "));
        line2_spans.push(Span::styled(
            format!("[stats {}]", sym),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        line2_spans.push(Span::raw(" "));
        line2_spans.push(Span::styled(
            source_summary(s),
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Db — periodic.
    if host.db_enabled {
        let s = host.source(SourceKind::Db);
        let (sym, color) = source_badge(s);
        line2_spans.push(Span::raw(" "));
        line2_spans.push(Span::styled(
            format!("[db {}]", sym),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        line2_spans.push(Span::raw(" "));
        line2_spans.push(Span::styled(
            source_summary(s),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let line2 = Line::from(line2_spans);

    // Третья строка — toast (приоритет) или первая ошибка любого источника.
    let line3 = if let Some(t) = &app.toast {
        let color = if t.is_error { Color::Red } else { Color::Green };
        Line::from(Span::styled(t.msg.clone(), Style::default().fg(color)))
    } else if let Some((kind, err)) = first_source_error(app) {
        Line::from(Span::styled(
            format!("⚠ {}: {}", kind.label(), short_one_line(&err, 200)),
            Style::default().fg(Color::Red),
        ))
    } else {
        Line::from("")
    };

    let para = Paragraph::new(vec![line1, line2, line3]).wrap(Wrap { trim: true });
    f.render_widget(para, area);
}

/// Иконка состояния источника + цвет.
fn source_badge(s: &SourceState) -> (&'static str, Color) {
    if s.last_error.is_some() && s.last_ok.is_none() {
        ("✗", Color::Red)
    } else if s.is_stale(Duration::from_secs(10)) {
        ("⚠", Color::Yellow)
    } else if s.last_ok.is_some() {
        ("✓", Color::Green)
    } else {
        ("…", Color::DarkGray)
    }
}

/// Краткая статистика источника для второй строки: "1.2s ago, 23ms".
fn source_summary(s: &SourceState) -> String {
    let age = s
        .age()
        .map(|d| {
            if d.as_secs() < 60 {
                format!("{}s", d.as_secs())
            } else {
                format!("{}m", d.as_secs() / 60)
            }
        })
        .unwrap_or_else(|| "—".into());
    let took = s
        .last_duration
        .map(|d| format!("{}ms", d.as_millis()))
        .unwrap_or_else(|| "—".into());
    let err_suffix = if s.consecutive_errors > 0 {
        format!(" err×{}", s.consecutive_errors)
    } else {
        String::new()
    };
    format!("{age} ago, {took}{err_suffix}")
}

/// Первый источник с ошибкой (для строки шапки).
fn first_source_error(app: &App) -> Option<(SourceKind, String)> {
    let host = app.focused();
    for kind in [SourceKind::Procs, SourceKind::Sys, SourceKind::Logs] {
        let s = host.source(kind);
        if let Some(e) = &s.last_error {
            return Some((kind, e.clone()));
        }
    }
    None
}

/// Бейдж для streaming-лога: символ + цвет + текстовая сводка.
///
/// Логика:
/// - connected=true  → `●` зелёный, сводка `streaming, last 3s ago, N lines`
/// - connected=false → `○` красный (если были реконнекты) или жёлтый (первое
///   подключение), сводка `reconnecting ×K`
fn log_badge(app: &App) -> (&'static str, Color, String) {
    let host = app.focused();
    let ls = &host.log_stream;
    let logs_state = host.source(SourceKind::Logs);

    if ls.connected {
        let last_line = logs_state
            .age()
            .map(|d| {
                if d.as_secs() < 60 {
                    format!("last {}s ago", d.as_secs())
                } else {
                    format!("last {}m ago", d.as_secs() / 60)
                }
            })
            .unwrap_or_else(|| "no lines yet".into());
        let reconn = if ls.reconnects > 0 {
            format!(", reconn ×{}", ls.reconnects)
        } else {
            String::new()
        };
        (
            "●",
            Color::Green,
            format!("streaming, {last_line}{reconn}"),
        )
    } else if ls.reconnects == 0 {
        // ещё не подключались
        ("○", Color::Yellow, "connecting…".to_string())
    } else {
        let err = ls
            .last_error
            .as_deref()
            .map(|e| short_one_line(e, 80))
            .unwrap_or_default();
        (
            "○",
            Color::Red,
            format!("reconnecting ×{} ({err})", ls.reconnects),
        )
    }
}

/// Полоса диагнозов между tabs и body. Каждый диагноз — одна строка:
/// `[CRIT] title — detail (sources: stats+db)`. Цвет по severity.
fn draw_diagnoses(f: &mut Frame, diagnoses: &[crate::diagnose::Diagnosis], area: Rect) {
    use crate::diagnose::Diagnosis;
    let lines: Vec<Line> = diagnoses
        .iter()
        .take(3)
        .map(|d: &Diagnosis| {
            let (tag, color) = match d.severity {
                Severity::Critical => ("CRIT", Color::Red),
                Severity::Warning => ("WARN", Color::Yellow),
                Severity::Info => ("INFO", Color::Cyan),
            };
            Line::from(vec![
                Span::styled(
                    format!("[{}] ", tag),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    d.title.clone(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" — "),
                Span::raw(d.detail.clone()),
                Span::styled(
                    format!("  ({})", d.sources.join("+")),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();
    let para = Paragraph::new(lines).wrap(Wrap { trim: true });
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
        make("0 Overview", app.tab == Tab::Overview),
        Span::raw(" "),
        make("1 Processes", app.tab == Tab::Processes),
        Span::raw(" "),
        make("2 Graphs", app.tab == Tab::Graphs),
        Span::raw(" "),
        make("3 Logs", app.tab == Tab::Logs),
        Span::raw(" "),
        make("4 Internals", app.tab == Tab::Internals),
        Span::raw(" "),
        make("5 Database", app.tab == Tab::Database),
        Span::raw(" "),
        make("6 Probes", app.tab == Tab::Probes),
        Span::raw("    "),
        Span::styled(
            format!("[host {}/{}]", app.focused_host + 1, app.hosts.len()),
            Style::default().fg(Color::Yellow),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// v0.5a/b — overview по всем хостам. Сверху — таблица per-host;
/// внизу — aggregate-строка по всему флоту (sum CPU, max load, sum queue,
/// total active diagnoses).
fn draw_overview(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(4)])
        .split(area);

    draw_overview_table(f, app, chunks[0]);
    draw_overview_aggregate(f, app, chunks[1]);
}

fn draw_overview_table(f: &mut Frame, app: &App, area: Rect) {
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
            // Светофор: счётчик здоровых источников vs всего источников.
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
                enabled && h.source(**k).last_ok.is_some() && !h.source(**k).is_stale(Duration::from_secs(15))
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
            let diag_count = diagnose(h).len();

            // Два маркера: cursor (для выбора) и focus (текущий открытый хост).
            // `▸` — где сейчас курсор, `●` — какой хост в drill-down открыт.
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
                Cell::from(format!("{:>4.0}%", mem))
                    .style(Style::default().fg(pct_color(mem))),
                Cell::from(format!("{:>5.2}", load))
                    .style(Style::default().fg(load_color(load))),
                Cell::from(queue),
                Cell::from(if diag_count > 0 {
                    format!("{}!", diag_count)
                } else {
                    "0".into()
                })
                .style(Style::default().fg(if diag_count > 0 {
                    Color::Red
                } else {
                    Color::Green
                })),
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
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Hosts  ({}) — Ctrl-N/P switch focus ", app.hosts.len())),
        );
    f.render_widget(table, area);
}

/// Fleet-level summary под таблицей Overview. Покажет, как чувствует себя
/// весь пул серверов в одной строке.
fn draw_overview_aggregate(f: &mut Frame, app: &App, area: Rect) {
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
    let total_diag: usize = app
        .hosts
        .iter()
        .map(|h| crate::diagnose::diagnose(h).len())
        .sum();
    let critical_diag: usize = app
        .hosts
        .iter()
        .flat_map(|h| crate::diagnose::diagnose(h))
        .filter(|d| d.severity == crate::diagnose::Severity::Critical)
        .count();

    let healthy = app
        .hosts
        .iter()
        .filter(|h| {
            let stale = h.source(SourceKind::Procs).is_stale(Duration::from_secs(15));
            !stale
        })
        .count();

    let line1 = Line::from(vec![
        Span::raw("Fleet "),
        Span::styled(
            format!("{}/{}", healthy, app.hosts.len()),
            Style::default().fg(if healthy == app.hosts.len() {
                Color::Green
            } else {
                Color::Yellow
            }).add_modifier(Modifier::BOLD),
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
            Style::default().fg(if total_diag > 0 {
                Color::Yellow
            } else {
                Color::Green
            }),
        ),
        Span::raw("  ("),
        Span::styled(
            format!("{} critical", critical_diag),
            Style::default().fg(if critical_diag > 0 {
                Color::Red
            } else {
                Color::DarkGray
            }),
        ),
        Span::raw(")"),
    ]);

    let para = Paragraph::new(vec![line1, line2]).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Fleet aggregate "),
    );
    f.render_widget(para, area);
}

fn draw_processes(f: &mut Frame, app: &App, area: Rect) {
    let host = app.focused();
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    // Левая колонка: агрегаты по ролям + сравнение с busy% от самого сервера.
    // Ключевая UX: расхождение «busy от stats высокий, CPU от ps низкий»
    // показывает, что процесс ждёт (lock/DB/IO), а не считает.
    let header = Row::new(vec![
        Cell::from("Role"),
        Cell::from("Cnt"),
        Cell::from("CPU%"),     // из ps (что реально жрёт)
        Cell::from("RSS"),
        Cell::from("BusyPs"),   // % форков, которые сейчас не idle (из proctitle)
        Cell::from("BusyZbx"),  // busy.avg из zabbix.stats — что говорит сам сервер
        Cell::from("Δ"),        // разница BusyZbx - BusyPs (положительное = wait)
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
            // Stats может слегка по-другому именовать роль (с пробелами/тире).
            // Простой lookup по точному совпадению — для большинства ролей
            // работает; если нет совпадения, ячейка пустая.
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
                .style(delta.map(|d| delta_color(d)).unwrap_or_default()),
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

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(" Roles  (ps vs zabbix.stats) "));
    f.render_widget(table, layout[0]);

    // Правая колонка: мини-графики
    draw_sparklines(f, app, layout[1]);
}

fn draw_graphs(f: &mut Frame, app: &App, area: Rect) {
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

fn draw_sparklines(f: &mut Frame, app: &App, area: Rect) {
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

    draw_sparkline(f, chunks[0], " CPU sum % ", &host.history.cpu_total, 100.0, Color::Cyan);
    draw_sparkline(f, chunks[1], " Mem % ", &host.history.mem_used_pct, 100.0, Color::Green);
    draw_sparkline(f, chunks[2], " Load 1m ", &host.history.load1, 8.0, Color::Yellow);

    // Внизу — гейджи мгновенных значений
    let g_area = chunks[3];
    let gauges = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)])
        .split(g_area);

    let cpu = (host.cpu_sum().min(100.0).max(0.0) / 100.0 * 100.0) as u16;
    let mem = host.sys.mem_used_pct().clamp(0.0, 100.0) as u16;
    let swap = host.sys.swap_used_pct().clamp(0.0, 100.0) as u16;

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

fn draw_internals(f: &mut Frame, app: &App, area: Rect) {
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

    // Шапка-идентичность сервера
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
            Span::styled(
                format!("   vps {:.0}", v),
                Style::default().fg(Color::Cyan),
            )
        } else {
            Span::raw("")
        },
    ]))
    .block(Block::default().borders(Borders::ALL).title(" Server "));
    f.render_widget(ident, layout[0]);

    // Таблица процессов из stats: busy avg/max/min + count.
    let header = Row::new(vec![
        Cell::from("Process type"),
        Cell::from("Cnt"),
        Cell::from("Busy avg"),
        Cell::from("Busy max"),
        Cell::from("Busy min"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let mut rows: Vec<(String, &crate::zbxstats::ProcessStats)> = stats
        .process
        .iter()
        .map(|(k, v)| (k.clone(), v))
        .collect();
    // Сортировка по busy.avg по убыванию — самые «горячие» сверху.
    rows.sort_by(|a, b| b.1.busy.avg.partial_cmp(&a.1.busy.avg).unwrap_or(std::cmp::Ordering::Equal));

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

    // Кэши: write/read/value — используем gauge-ы.
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

/// v0.7 — синтетические пробы. Глобальные (не зависят от focused host).
fn draw_probes(f: &mut Frame, app: &App, area: Rect) {
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
                Cell::from(p.name.clone())
                    .style(Style::default().add_modifier(Modifier::BOLD)),
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

fn draw_database(f: &mut Frame, app: &App, area: Rect) {
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
                Cell::from(format!("{:>6.1}s", q.age_sec))
                    .style(Style::default().fg(age_color)),
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
            Cell::from(t.pretty.clone())
                .style(Style::default().add_modifier(Modifier::BOLD)),
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

fn draw_logs(f: &mut Frame, app: &App, area: Rect) {
    let host = app.focused();
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

fn draw_footer(f: &mut Frame, _app: &App, area: Rect) {
    let line = Line::from(vec![
        Span::styled(" [Tab] ", Style::default().fg(Color::Cyan)),
        Span::raw("tab  "),
        Span::styled("[^N/^P] ", Style::default().fg(Color::Cyan)),
        Span::raw("host  "),
        Span::styled("[^G] ", Style::default().fg(Color::Cyan)),
        Span::raw("picker  "),
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
        Span::styled("[L] ", Style::default().fg(Color::Cyan)),
        Span::raw("log-reconn  "),
        Span::styled("[q] ", Style::default().fg(Color::Cyan)),
        Span::raw("quit"),
    ]);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(Color::Reset)),
        area,
    );
}

fn draw_runtime_modal(f: &mut Frame, app: &App) {
    let host = app.focused();
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

    let title = if host.use_sudo {
        " Runtime control  [sudo -n]  ↑↓ select  Enter run  Esc close "
    } else {
        " Runtime control  ↑↓ select  Enter run  Esc close "
    };
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(list, area);
}

/// v0.5c — fuzzy host-picker overlay. Сверху поле запроса, ниже —
/// ранжированный список хостов с маркером курсора. Полезно на 20+ хостах,
/// где Ctrl-N/P перебирать долго.
fn draw_host_picker(f: &mut Frame, app: &App) {
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

    // Поле запроса.
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

    // Список совпадений.
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

/// Цвет колонки Δ: красный — сервер сильно «занят», ps этого не видит
/// (типичный признак ожидания lock/DB/IO). Зелёный — busy и CPU сходятся.
fn delta_color(d: f32) -> Style {
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
