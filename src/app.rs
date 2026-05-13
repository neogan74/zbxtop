//! Состояние приложения и логика обновления.

use crate::collectors::{LogLine, RuntimeCmd, SysStats, ZbxProc, ZbxRoleAgg};
use std::collections::VecDeque;

const HISTORY_LEN: usize = 240; // 8 минут при 2-сек тике

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Processes,
    Graphs,
    Logs,
}

#[derive(Clone, Debug)]
pub struct History {
    pub cpu_total: VecDeque<f64>, // суммарный CPU по zabbix_server форкам, %
    pub mem_used_pct: VecDeque<f64>,
    pub load1: VecDeque<f64>,
    pub queue_proxy: VecDeque<f64>, // зарезервировано
}

impl Default for History {
    fn default() -> Self {
        Self {
            cpu_total: VecDeque::with_capacity(HISTORY_LEN),
            mem_used_pct: VecDeque::with_capacity(HISTORY_LEN),
            load1: VecDeque::with_capacity(HISTORY_LEN),
            queue_proxy: VecDeque::with_capacity(HISTORY_LEN),
        }
    }
}

impl History {
    pub fn push(&mut self, cpu: f64, mem: f64, load: f64) {
        push_ring(&mut self.cpu_total, cpu);
        push_ring(&mut self.mem_used_pct, mem);
        push_ring(&mut self.load1, load);
    }
}

fn push_ring(v: &mut VecDeque<f64>, x: f64) {
    if v.len() == HISTORY_LEN {
        v.pop_front();
    }
    v.push_back(x);
}

#[derive(Clone, Debug, Default)]
pub struct Toast {
    pub msg: String,
    pub is_error: bool,
    pub ttl_ticks: u8,
}

#[derive(Clone, Debug)]
pub struct App {
    pub host: String,
    pub tab: Tab,
    pub paused: bool,
    pub show_runtime_menu: bool,
    pub runtime_cursor: usize,
    pub runtime_choices: Vec<RuntimeCmd>,
    pub use_sudo: bool,
    pub log_path: String,
    pub log_filter: String,
    pub editing_filter: bool,

    pub procs: Vec<ZbxProc>,
    pub roles: Vec<ZbxRoleAgg>,
    pub sys: SysStats,
    pub logs: Vec<LogLine>,

    pub history: History,
    pub last_refresh: Option<chrono::DateTime<chrono::Local>>,
    pub last_error: Option<String>,
    pub toast: Option<Toast>,
}

impl App {
    pub fn new(host: String, log_path: String, use_sudo: bool) -> Self {
        Self {
            host,
            tab: Tab::Processes,
            paused: false,
            show_runtime_menu: false,
            runtime_cursor: 0,
            runtime_choices: vec![
                RuntimeCmd::LogLevelIncrease,
                RuntimeCmd::LogLevelDecrease,
                RuntimeCmd::ConfigCacheReload,
                RuntimeCmd::SnmpCacheReload,
                RuntimeCmd::HousekeeperExecute,
                RuntimeCmd::Diaginfo,
            ],
            use_sudo,
            log_path,
            log_filter: String::new(),
            editing_filter: false,
            procs: vec![],
            roles: vec![],
            sys: SysStats::default(),
            logs: vec![],
            history: History::default(),
            last_refresh: None,
            last_error: None,
            toast: None,
        }
    }

    pub fn next_tab(&mut self) {
        self.tab = match self.tab {
            Tab::Processes => Tab::Graphs,
            Tab::Graphs => Tab::Logs,
            Tab::Logs => Tab::Processes,
        };
    }

    pub fn set_toast(&mut self, msg: impl Into<String>, is_error: bool) {
        self.toast = Some(Toast {
            msg: msg.into(),
            is_error,
            ttl_ticks: 4, // ~8 секунд при 2-сек тике
        });
    }

    pub fn tick_decay(&mut self) {
        if let Some(t) = self.toast.as_mut() {
            if t.ttl_ticks == 0 {
                self.toast = None;
            } else {
                t.ttl_ticks -= 1;
            }
        }
    }

    pub fn cpu_sum(&self) -> f64 {
        self.procs.iter().map(|p| p.cpu as f64).sum()
    }
}
