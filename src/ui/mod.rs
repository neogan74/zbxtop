//! Ratatui rendering, split by screen area: chrome (header/tabs/footer),
//! one module per tab, modals, and shared helpers.

mod chrome;
mod database;
mod graphs;
mod internals;
mod logs;
mod modal;
mod overview;
mod probes;
mod processes;
mod util;

use chrome::{draw_diagnoses, draw_footer, draw_header, draw_tabs};
use database::draw_database;
use graphs::draw_graphs;
use internals::draw_internals;
use logs::draw_logs;
use modal::{draw_host_picker, draw_runtime_modal};
use overview::draw_overview;
use probes::draw_probes;
use processes::draw_processes;

use crate::app::{App, Tab};
use crate::diagnose::{diagnose, Diagnosis};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

pub fn draw(f: &mut Frame, app: &App) {
    // Compute diagnoses once per frame; reused by header banner, overview table, and aggregate.
    let per_host_diagnoses: Vec<Vec<Diagnosis>> = app.hosts.iter().map(diagnose).collect();

    // Cross-source diagnoses for all hosts, prefixed with [name]. We show
    // at most 3 — the most severe. Row height is dynamic.
    let mut diagnoses: Vec<_> = app
        .hosts
        .iter()
        .zip(&per_host_diagnoses)
        .flat_map(|(h, ds)| {
            let name = h.name.clone();
            ds.iter().cloned().map(move |mut d| {
                d.title = format!("[{}] {}", name, d.title);
                d
            })
        })
        .collect();
    diagnoses.sort_by_key(|d| std::cmp::Reverse(d.severity));
    let total_diag_count = diagnoses.len();
    diagnoses.truncate(3);
    let diag_height = diagnoses.len() as u16;

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),           // header
            Constraint::Length(1),           // tabs
            Constraint::Length(diag_height), // diagnoses, 0-3 rows
            Constraint::Min(0),              // body
            Constraint::Length(1),           // footer
        ])
        .split(f.area());

    draw_header(f, app, root[0]);
    draw_tabs(f, app, root[1]);
    if diag_height > 0 {
        draw_diagnoses(f, &diagnoses, total_diag_count, root[2]);
    }
    match app.tab {
        Tab::Overview => draw_overview(f, app, &per_host_diagnoses, root[3]),
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
    // v0.5c: host-picker is drawn on top of everything, including the runtime
    // modal, so Ctrl-G works even when the runtime control is open.
    if app.show_host_picker {
        draw_host_picker(f, app);
    }
}
