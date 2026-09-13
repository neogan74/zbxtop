//! Frame chrome: header with source badges, tab bar, diagnoses strip, footer.

use super::util::*;
use crate::app::{App, Tab};
use crate::diagnose::Severity;
use crate::source::{SourceKind, SourceState};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};
use std::time::Duration;

pub(super) fn draw_header(f: &mut Frame, app: &App, area: Rect) {
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
        let pct = app
            .replay_progress
            .load(std::sync::atomic::Ordering::Relaxed) as f32
            / 10.0;
        let replay_paused = app
            .replay_paused_flag
            .load(std::sync::atomic::Ordering::Relaxed);
        let marker = if replay_paused { "⏸ " } else { "▶ " };
        let pause_tag = if replay_paused { " [PAUSED]" } else { "" };
        Some((
            format!("{marker}REPLAY {:.1}%{pause_tag}", pct),
            Color::Blue,
        ))
    } else if app.recorded_events > 0 {
        Some((format!("● REC ({} ev)", app.recorded_events), Color::Red))
    } else {
        None
    };

    let line1 = Line::from(vec![
        Span::styled(
            "ztop ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("@ "),
        Span::styled(&host.name, Style::default().fg(Color::Yellow)),
        Span::raw("   load "),
        Span::styled(
            format!(
                "{:.2} {:.2} {:.2}",
                host.sys.load1, host.sys.load5, host.sys.load15
            ),
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

    // Second header row: status of each source.
    let mut line2_spans = Vec::with_capacity(16);

    // Procs and Sys — periodic sources: badge based on SourceState (age + duration).
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

    // Logs — streaming: badge based on log_stream.connected, not age (silence
    // in the log is normal; a failure is when the stream drops).
    let (sym, color, summary) = log_badge(app);
    line2_spans.push(Span::raw(" "));
    line2_spans.push(Span::styled(
        format!("[log {}]", sym),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    line2_spans.push(Span::raw(" "));
    line2_spans.push(Span::styled(summary, Style::default().fg(Color::DarkGray)));

    // Stats — periodic; shown only if the transport is enabled.
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

    // Third row — toast (priority) or the first error from any source.
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

/// Source status icon + color.
pub(super) fn source_badge(s: &SourceState) -> (&'static str, Color) {
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

/// Brief source statistics for the second row: "1.2s ago, 23ms".
pub(super) fn source_summary(s: &SourceState) -> String {
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

/// First source with an error (for the header row).
pub(super) fn first_source_error(app: &App) -> Option<(SourceKind, String)> {
    let host = app.focused();
    for kind in [SourceKind::Procs, SourceKind::Sys, SourceKind::Logs] {
        let s = host.source(kind);
        if let Some(e) = &s.last_error {
            return Some((kind, e.clone()));
        }
    }
    None
}

/// Badge for the streaming log: symbol + color + text summary.
///
/// Logic:
/// - connected=true  → `●` green, summary `streaming, last 3s ago, N lines`
/// - connected=false → `○` red (if reconnects have occurred) or yellow (first
///   connection attempt), summary `reconnecting ×K`
pub(super) fn log_badge(app: &App) -> (&'static str, Color, String) {
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
        ("●", Color::Green, format!("streaming, {last_line}{reconn}"))
    } else if ls.reconnects == 0 {
        // not yet connected
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

/// Diagnosis strip between tabs and body. Each diagnosis is one line with a
/// tinted background: dark red for Critical, dark yellow for Warning.
/// If more than 3 diagnoses exist, a `+N more` suffix is appended to the last line.
pub(super) fn draw_diagnoses(
    f: &mut Frame,
    diagnoses: &[crate::diagnose::Diagnosis],
    total: usize,
    area: Rect,
) {
    let width = area.width as usize;
    let shown = diagnoses.len();

    for (idx, d) in diagnoses.iter().enumerate() {
        let (tag, fg, bg) = match d.severity {
            Severity::Critical => ("CRIT", Color::White, Color::Rgb(90, 20, 20)),
            Severity::Warning => ("WARN", Color::Rgb(255, 220, 0), Color::Rgb(60, 50, 10)),
            Severity::Info => ("INFO", Color::Cyan, Color::Rgb(10, 40, 60)),
        };

        let sources = format!("  ({})", d.sources.join("+"));
        let prefix = format!(" [{tag}] ");
        let title_sep = format!("{} — ", d.title);

        // Build the suffix shown on the last visible line when there are hidden diagnoses.
        let overflow = if idx == shown - 1 && total > shown {
            format!("  … +{} more", total - shown)
        } else {
            String::new()
        };

        // Reserve space for sources and overflow; truncate detail to fit.
        let fixed = prefix.len() + title_sep.len() + sources.len() + overflow.len();
        let detail_max = width.saturating_sub(fixed);
        let detail: String = d.detail.chars().take(detail_max).collect();

        let line = Line::from(vec![
            Span::styled(
                prefix,
                Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                title_sep,
                Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(detail, Style::default().fg(Color::White).bg(bg)),
            Span::styled(sources, Style::default().fg(Color::DarkGray).bg(bg)),
            Span::styled(overflow, Style::default().fg(Color::DarkGray).bg(bg)),
            // Pad to full width so the background tint covers the entire row.
            Span::styled(" ".repeat(width), Style::default().bg(bg)),
        ]);

        let row_area = Rect {
            x: area.x,
            y: area.y + idx as u16,
            width: area.width,
            height: 1,
        };
        f.render_widget(Paragraph::new(line), row_area);
    }
}

pub(super) fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
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

pub(super) fn draw_footer(f: &mut Frame, _app: &App, area: Rect) {
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
