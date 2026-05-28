//! Состояние приложения и логика обновления.
//!
//! v0.5a — мульти-хостовая модель. `App` хранит `Vec<HostState>` (по одному
//! на отслеживаемый Zabbix-сервер) и `focused_host: usize` — индекс хоста,
//! который сейчас в детальном просмотре. Глобальные UI-флаги (paused,
//! editing_filter, show_runtime_menu и т.п.) остаются на `App`.

use crate::collectors::{LogLine, RuntimeCmd, SysStats, ZbxProc, ZbxRoleAgg};
use crate::db::DbStats;
use crate::probes::ProbeState;
use crate::source::{CollectorMsg, LogStreamStatus, SourceKind, SourceState};
use crate::zbxstats::ZabbixStats;
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

const HISTORY_LEN: usize = 240;
const MAX_LOG_LINES: usize = 5_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Processes,
    Graphs,
    Logs,
    Internals,
    Database,
    /// v0.7 — синтетические пробы (TCP/DNS/PG из машины ztop).
    Probes,
}

#[derive(Clone, Debug)]
pub struct History {
    pub cpu_total: VecDeque<f64>,
    pub mem_used_pct: VecDeque<f64>,
    pub load1: VecDeque<f64>,
    pub queue_proxy: VecDeque<f64>,
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

/// Всё, что приходит с одного хоста. Список таких в `App.hosts`.
#[derive(Clone, Debug)]
pub struct HostState {
    /// Имя для UI (произвольное, не обязательно совпадает с ssh).
    pub name: String,
    /// SSH-таргет (для runtime control в шапке хоста).
    pub ssh_host: String,
    pub log_path: String,
    pub use_sudo: bool,
    pub stats_enabled: bool,
    pub db_enabled: bool,

    pub procs: Vec<ZbxProc>,
    pub roles: Vec<ZbxRoleAgg>,
    pub sys: SysStats,
    pub logs: Vec<LogLine>,

    pub history: History,
    pub sources: HashMap<SourceKind, SourceState>,
    pub log_stream: LogStreamStatus,
    pub stats: Option<ZabbixStats>,
    pub db: Option<DbStats>,
}

impl HostState {
    pub fn new(
        name: String,
        ssh_host: String,
        log_path: String,
        use_sudo: bool,
        stats_enabled: bool,
        db_enabled: bool,
    ) -> Self {
        Self {
            name,
            ssh_host,
            log_path,
            use_sudo,
            stats_enabled,
            db_enabled,
            procs: vec![],
            roles: vec![],
            sys: SysStats::default(),
            logs: vec![],
            history: History::default(),
            sources: HashMap::from([
                (SourceKind::Procs, SourceState::default()),
                (SourceKind::Sys, SourceState::default()),
                (SourceKind::Logs, SourceState::default()),
                (SourceKind::Stats, SourceState::default()),
                (SourceKind::Db, SourceState::default()),
            ]),
            log_stream: LogStreamStatus::default(),
            stats: None,
            db: None,
        }
    }

    pub fn apply_msg(&mut self, msg: CollectorMsg) {
        // v0.6.1: Reset — синтетический сигнал из replay backward-seek.
        // Сбрасываем рантайм-стейт, но сохраняем имя/SSH/флаги конфигурации.
        if matches!(msg, CollectorMsg::Reset) {
            self.procs.clear();
            self.roles.clear();
            self.sys = SysStats::default();
            self.logs.clear();
            self.history = History::default();
            self.log_stream = LogStreamStatus::default();
            self.stats = None;
            self.db = None;
            for state in self.sources.values_mut() {
                *state = SourceState::default();
            }
            return;
        }

        let kind = msg.kind();
        let now = Instant::now();
        let st = self.sources.entry(kind).or_default();
        st.last_attempt = Some(now);

        match msg {
            CollectorMsg::Procs { result, took } => {
                st.last_duration = Some(took);
                match result {
                    Ok(procs) => {
                        self.roles = crate::collectors::aggregate_roles(&procs);
                        self.procs = procs;
                        st.last_ok = Some(now);
                        st.consecutive_errors = 0;
                        st.last_error = None;
                    }
                    Err(e) => {
                        st.consecutive_errors = st.consecutive_errors.saturating_add(1);
                        st.last_error = Some(format!("{e:#}"));
                    }
                }
            }
            CollectorMsg::Sys { result, took } => {
                st.last_duration = Some(took);
                match result {
                    Ok(s) => {
                        self.sys = s;
                        st.last_ok = Some(now);
                        st.consecutive_errors = 0;
                        st.last_error = None;
                    }
                    Err(e) => {
                        st.consecutive_errors = st.consecutive_errors.saturating_add(1);
                        st.last_error = Some(format!("{e:#}"));
                    }
                }
            }
            CollectorMsg::Stats { result, took } => {
                st.last_duration = Some(took);
                match result {
                    Ok(s) => {
                        self.stats = Some(s);
                        st.last_ok = Some(now);
                        st.consecutive_errors = 0;
                        st.last_error = None;
                    }
                    Err(e) => {
                        st.consecutive_errors = st.consecutive_errors.saturating_add(1);
                        st.last_error = Some(format!("{e:#}"));
                    }
                }
            }
            CollectorMsg::Db { result, took } => {
                st.last_duration = Some(took);
                match result {
                    Ok(s) => {
                        self.db = Some(s);
                        st.last_ok = Some(now);
                        st.consecutive_errors = 0;
                        st.last_error = None;
                    }
                    Err(e) => {
                        st.consecutive_errors = st.consecutive_errors.saturating_add(1);
                        st.last_error = Some(format!("{e:#}"));
                    }
                }
            }
            CollectorMsg::LogStreamLine(line) => {
                self.logs.push(line);
                if self.logs.len() > MAX_LOG_LINES {
                    let excess = self.logs.len() - MAX_LOG_LINES;
                    self.logs.drain(0..excess);
                }
                st.last_ok = Some(now);
                st.consecutive_errors = 0;
                st.last_error = None;
            }
            CollectorMsg::Reset => {
                // Покрыто early-return-ом выше; ветка существует для
                // exhaustiveness match.
                unreachable!("Reset handled before sources.entry()");
            }
            CollectorMsg::LogStreamStatus(status) => {
                self.log_stream.connected = status.connected;
                self.log_stream.reconnects =
                    self.log_stream.reconnects.max(status.reconnects);
                self.log_stream.last_error = status.last_error.clone();

                if status.connected {
                    st.last_ok = Some(now);
                    st.last_error = None;
                    st.consecutive_errors = 0;
                } else if let Some(err) = status.last_error {
                    st.consecutive_errors = st.consecutive_errors.saturating_add(1);
                    st.last_error = Some(err);
                }
            }
        }

        self.history.push(
            self.cpu_sum(),
            self.sys.mem_used_pct() as f64,
            self.sys.load1 as f64,
        );
    }

    pub fn source(&self, kind: SourceKind) -> &SourceState {
        static EMPTY: SourceState = SourceState {
            last_ok: None,
            last_attempt: None,
            last_duration: None,
            last_error: None,
            consecutive_errors: 0,
        };
        self.sources.get(&kind).unwrap_or(&EMPTY)
    }

    pub fn cpu_sum(&self) -> f64 {
        self.procs.iter().map(|p| p.cpu as f64).sum()
    }
}

/// Глобальный state приложения. UI-state (вкладки, фильтры, тосты)
/// храним здесь; всё, что приходит из коллекторов конкретного хоста — в
/// соответствующем `HostState`.
///
/// WARNING: `derive(Clone)` is kept for convenience but cloning `App` shares the
/// `Arc<AtomicU32>`/`Arc<AtomicBool>` replay state fields — both copies will observe
/// and mutate the same counters. `App` is never cloned today; if you add a clone site,
/// ensure the shared replay state is intentional or use `Arc::new(AtomicU32::new(0))`
/// to give the clone its own copy.
#[derive(Clone, Debug)]
pub struct App {
    pub hosts: Vec<HostState>,
    pub focused_host: usize,

    pub tab: Tab,
    /// Курсор в Overview-табе: индекс хоста, на котором сейчас «подсветка».
    /// При входе на Overview инициализируется в focused_host. Enter переносит
    /// focused_host → overview_cursor и переключается на drill-down.
    pub overview_cursor: usize,
    pub paused: bool,
    pub show_runtime_menu: bool,
    pub runtime_cursor: usize,
    pub runtime_choices: Vec<RuntimeCmd>,
    pub log_filter: String,
    pub editing_filter: bool,
    pub last_refresh: Option<chrono::DateTime<chrono::Local>>,
    pub toast: Option<Toast>,

    /// v0.6: индикаторы режима записи/воспроизведения.
    /// Главный source-of-truth по replay-режиму: блокирует runtime control,
    /// UI рисует другой бейдж.
    pub is_replay: bool,
    /// Кол-во событий, записанных recorder-ом (для UI-индикатора).
    pub recorded_events: u64,

    /// v0.6.1: канал команд для replay_loop (None если не replay).
    /// `Space`/`n`/`>`/`<` пушат сюда `ReplayCmd`.
    pub replay_cmd_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::record::ReplayCmd>>,
    /// Прогресс воспроизведения в промилле (0..=1000). Читается атомарно
    /// из replay_loop; UI делит на 10.0 для процента.
    pub replay_progress: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// True когда replay-loop сейчас на паузе. Тоже атомарно.
    pub replay_paused_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,

    /// v0.7: глобальные пробы (не привязаны к host). Vec индексируется
    /// тем же индексом, что и в hosts.toml [[probe]] списке.
    pub probes: Vec<ProbeState>,

    /// v0.5c: модал-выборщик хостов с fuzzy-search. Открывается по Ctrl-G,
    /// показывает поле запроса + отфильтрованный список. На крупных
    /// деплоях (20+ хостов) удобнее, чем Ctrl-N/P перебор.
    pub show_host_picker: bool,
    pub host_picker_query: String,
    pub host_picker_cursor: usize,
}

impl App {
    pub fn new(hosts: Vec<HostState>) -> Self {
        assert!(!hosts.is_empty(), "App requires at least one host");
        Self {
            hosts,
            focused_host: 0,
            tab: Tab::Overview,
            overview_cursor: 0,
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
            log_filter: String::new(),
            editing_filter: false,
            last_refresh: None,
            toast: None,
            is_replay: false,
            recorded_events: 0,
            replay_cmd_tx: None,
            replay_progress: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            replay_paused_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            probes: Vec::new(),
            show_host_picker: false,
            host_picker_query: String::new(),
            host_picker_cursor: 0,
        }
    }

    /// v0.5c: открыть host-picker. Сбрасывает query и курсор.
    pub fn open_host_picker(&mut self) {
        self.show_host_picker = true;
        self.host_picker_query.clear();
        self.host_picker_cursor = 0;
    }

    /// v0.5c: применить курсор picker-а как новый focused_host.
    pub fn host_picker_apply(&mut self) {
        let matches = self.host_picker_matches();
        if let Some(&host_idx) = matches.get(self.host_picker_cursor) {
            self.focused_host = host_idx;
            self.overview_cursor = host_idx;
        }
        self.show_host_picker = false;
    }

    /// v0.5c: вернуть список индексов хостов, совпавших с query, отсортированный
    /// по убыванию score (точные совпадения сверху). Пустой query → все хосты
    /// в исходном порядке.
    pub fn host_picker_matches(&self) -> Vec<usize> {
        if self.host_picker_query.is_empty() {
            return (0..self.hosts.len()).collect();
        }
        let q = self.host_picker_query.to_lowercase();
        let mut scored: Vec<(usize, i32)> = self
            .hosts
            .iter()
            .enumerate()
            .filter_map(|(i, h)| {
                let candidates = [h.name.to_lowercase(), h.ssh_host.to_lowercase()];
                candidates
                    .iter()
                    .filter_map(|c| fuzzy_score(&q, c))
                    .max()
                    .map(|s| (i, s))
            })
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored.into_iter().map(|(i, _)| i).collect()
    }
}

/// v0.5c: fuzzy subsequence match. Возвращает score (выше — лучше совпадение)
/// или None если query вообще не subsequence target-а.
///
/// Score = базовые очки за каждое совпадение + бонус за consecutive runs
/// + бонус за совпадение в начале. Это даёт нормальный ранкинг: "prod"
/// матчит "zbx-prod-01" сильнее, чем "p…r…o…d" сильно разбросанные.
pub fn fuzzy_score(query: &str, target: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().collect();
    let t: Vec<char> = target.chars().collect();
    let mut qi = 0usize;
    let mut score: i32 = 0;
    let mut prev_match: Option<usize> = None;
    for (ti, &c) in t.iter().enumerate() {
        if qi < q.len() && c == q[qi] {
            score += 10;
            if let Some(p) = prev_match {
                if p + 1 == ti {
                    score += 15; // consecutive bonus
                }
            }
            if ti == 0 {
                score += 20; // start-of-string bonus
            }
            prev_match = Some(ti);
            qi += 1;
        }
    }
    if qi == q.len() {
        Some(score - (t.len() as i32))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matches_subsequence() {
        assert!(fuzzy_score("prod", "zbx-prod-01").is_some());
        assert!(fuzzy_score("prd", "zbx-prod-01").is_some());
        assert!(fuzzy_score("xyz", "zbx-prod-01").is_none());
    }

    #[test]
    fn fuzzy_consecutive_beats_scattered() {
        let exact = fuzzy_score("prod", "zbx-prod-01").unwrap();
        let scattered = fuzzy_score("pro1", "p-r-o-1-x").unwrap();
        // consecutive "prod" должен получить больше очков, чем scattered
        assert!(exact > scattered);
    }

    #[test]
    fn fuzzy_empty_query_passes() {
        assert!(fuzzy_score("", "anything").is_some());
    }
}

impl App {
    /// v0.7: применить результат пробы.
    pub fn apply_probe(&mut self, msg: crate::probes::ProbeMsg) {
        if let Some(p) = self.probes.get_mut(msg.probe_idx) {
            p.total_runs = p.total_runs.saturating_add(1);
            match msg.result {
                Ok(lat) => {
                    p.last_ok = Some(Instant::now());
                    p.last_latency = Some(lat);
                    p.last_error = None;
                    p.consecutive_errors = 0;
                    p.total_ok = p.total_ok.saturating_add(1);
                }
                Err(e) => {
                    p.consecutive_errors = p.consecutive_errors.saturating_add(1);
                    p.last_error = Some(e);
                }
            }
        }
    }

    pub fn focused(&self) -> &HostState {
        &self.hosts[self.focused_host]
    }
    pub fn focused_mut(&mut self) -> &mut HostState {
        &mut self.hosts[self.focused_host]
    }

    pub fn next_host(&mut self) {
        if self.hosts.len() > 1 {
            self.focused_host = (self.focused_host + 1) % self.hosts.len();
        }
    }
    pub fn prev_host(&mut self) {
        if self.hosts.len() > 1 {
            self.focused_host = (self.focused_host + self.hosts.len() - 1) % self.hosts.len();
        }
    }

    pub fn overview_cursor_down(&mut self) {
        if self.overview_cursor + 1 < self.hosts.len() {
            self.overview_cursor += 1;
        }
    }
    pub fn overview_cursor_up(&mut self) {
        if self.overview_cursor > 0 {
            self.overview_cursor -= 1;
        }
    }
    /// Применить курсор: focused_host := cursor.
    pub fn overview_select(&mut self) {
        self.focused_host = self.overview_cursor;
    }

    pub fn next_tab(&mut self) {
        self.tab = match self.tab {
            Tab::Overview => Tab::Processes,
            Tab::Processes => Tab::Graphs,
            Tab::Graphs => Tab::Logs,
            Tab::Logs => Tab::Internals,
            Tab::Internals => Tab::Database,
            Tab::Database => Tab::Probes,
            Tab::Probes => Tab::Overview,
        };
    }

    pub fn set_toast(&mut self, msg: impl Into<String>, is_error: bool) {
        self.toast = Some(Toast {
            msg: msg.into(),
            is_error,
            ttl_ticks: 4,
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
}
