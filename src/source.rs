//! Per-collector задачи с независимым опросом, backoff и общим force-refresh.
//!
//! - Procs/Sys: периодический pull через `ssh ... 'cmd'` (tokio::process).
//! - Logs (v0.2b): long-lived ssh-процесс `tail -n N -F`, построчный стрим в
//!   mpsc-канал. Reconnect с экспоненциальным backoff. Так получаем real-time
//!   логи без поллинга — критично в момент инцидента.

use crate::collectors::{classify, fetch_procs, fetch_sys, LogLine, SysStats, ZbxProc};
use crate::db::{DbBackend, DbStats, DbTarget};
use crate::ssh::SshTarget;
use crate::zbxstats::{fetch_stats, ZabbixStats, ZbxStatsTarget};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceKind {
    Procs,
    Sys,
    Logs,
    Stats,
    Db,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Procs => "ps",
            Self::Sys => "sys",
            Self::Logs => "log",
            Self::Stats => "stats",
            Self::Db => "db",
        }
    }
}

/// Состояние одного источника данных (обновляется при получении CollectorMsg).
/// Для periodic-источников (Procs/Sys) — стандартная семантика: last_ok = время
/// последнего успешного запроса. Для streaming-источника Logs — last_ok = время
/// последней полученной строки лога (или подключения, если строк ещё не было).
#[derive(Clone, Debug, Default)]
pub struct SourceState {
    pub last_ok: Option<Instant>,
    pub last_attempt: Option<Instant>,
    pub last_duration: Option<Duration>,
    pub last_error: Option<String>,
    pub consecutive_errors: u32,
}

impl SourceState {
    pub fn is_stale(&self, threshold: Duration) -> bool {
        match self.last_ok {
            Some(t) => t.elapsed() > threshold,
            None => true,
        }
    }
    pub fn age(&self) -> Option<Duration> {
        self.last_ok.map(|t| t.elapsed())
    }
}

/// Состояние лог-стрима — отдельная структура, потому что у стрима другая
/// семантика (не периодический запрос, а живой подпроцесс).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LogStreamStatus {
    /// True, пока ssh-подпроцесс жив и читается stdout.
    pub connected: bool,
    /// Сколько раз перезапускали стрим с начала сессии.
    pub reconnects: u32,
    pub last_error: Option<String>,
}

/// Сообщение из коллектора в event-loop.
pub enum CollectorMsg {
    Procs {
        result: Result<Vec<ZbxProc>>,
        took: Duration,
    },
    Sys {
        result: Result<SysStats>,
        took: Duration,
    },
    /// Одна строка из streaming-лога.
    LogStreamLine(LogLine),
    /// Изменение статуса стрима (connect/disconnect/error).
    LogStreamStatus(LogStreamStatus),
    /// Снапшот zabbix.stats через trapper-порт.
    Stats {
        result: Result<ZabbixStats>,
        took: Duration,
    },
    /// Снапшот DB-метрик (PostgreSQL).
    Db {
        result: Result<DbStats>,
        took: Duration,
    },
    /// v0.6.1: синтетический сигнал — сбросить весь HostState на дефолт.
    /// Используется replay-loop-ом для backward-seek (rewind), чтобы после
    /// прыжка назад экран не показывал «будущие» данные.
    Reset,
}

impl CollectorMsg {
    pub fn kind(&self) -> SourceKind {
        match self {
            Self::Procs { .. } => SourceKind::Procs,
            Self::Sys { .. } => SourceKind::Sys,
            Self::LogStreamLine(_) | Self::LogStreamStatus(_) => SourceKind::Logs,
            Self::Stats { .. } => SourceKind::Stats,
            Self::Db { .. } => SourceKind::Db,
            // Reset не относится к конкретному источнику; возвращаем Procs
            // как наименее значимый — apply_msg обрабатывает Reset до того,
            // как вообще трогает sources HashMap.
            Self::Reset => SourceKind::Procs,
        }
    }
}

/// v0.5a — обёртка сообщений для маршрутизации в нужный `HostState`.
pub struct HostMsg {
    pub host_idx: usize,
    pub msg: CollectorMsg,
}

pub struct CollectorHandles {
    /// Глобальный force-refresh. На v0.5a один Notify на все хосты —
    /// `r` пробуждает collectors всех хостов одновременно.
    pub refresh: Arc<Notify>,
    pub log_reconnect: Arc<Notify>,
    handles: Vec<JoinHandle<()>>,
}

impl CollectorHandles {
    pub fn shutdown(self) {
        for h in self.handles {
            h.abort();
        }
    }
}

/// Параметры запуска коллекторов для одного хоста.
pub struct HostSpawn {
    pub host_idx: usize,
    pub ssh: SshTarget,
    pub log_path: String,
    pub log_lines: usize,
    pub stats: Option<ZbxStatsTarget>,
    pub db: Option<DbTarget>,
}

pub fn spawn_collectors(
    hosts: Vec<HostSpawn>,
    base_interval: Duration,
    tx: mpsc::Sender<HostMsg>,
) -> CollectorHandles {
    let refresh = Arc::new(Notify::new());
    let log_reconnect = Arc::new(Notify::new());
    let mut handles = Vec::new();

    for h in hosts {
        handles.push(spawn_procs(
            h.host_idx,
            h.ssh.clone(),
            tx.clone(),
            refresh.clone(),
            base_interval,
        ));
        handles.push(spawn_sys(
            h.host_idx,
            h.ssh.clone(),
            tx.clone(),
            refresh.clone(),
            base_interval,
        ));
        handles.push(spawn_log_stream(
            h.host_idx,
            h.ssh.clone(),
            h.log_path,
            h.log_lines,
            tx.clone(),
            log_reconnect.clone(),
        ));
        if let Some(st) = h.stats {
            handles.push(spawn_stats(
                h.host_idx,
                st,
                tx.clone(),
                refresh.clone(),
                base_interval,
            ));
        }
        if let Some(db) = h.db {
            let db_interval = base_interval.saturating_mul(2).max(Duration::from_secs(5));
            handles.push(spawn_db(
                h.host_idx,
                db,
                tx.clone(),
                refresh.clone(),
                db_interval,
            ));
        }
    }

    CollectorHandles {
        refresh,
        log_reconnect,
        handles,
    }
}

// ---- periodic loops (procs, sys) ----

const MAX_BACKOFF: Duration = Duration::from_secs(30);

fn next_interval(current: Duration, base: Duration, errored: bool) -> Duration {
    if errored {
        (current.saturating_mul(2)).min(MAX_BACKOFF).max(base)
    } else {
        base
    }
}

fn spawn_procs(
    host_idx: usize,
    target: SshTarget,
    tx: mpsc::Sender<HostMsg>,
    refresh: Arc<Notify>,
    base: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = base;
        loop {
            let start = Instant::now();
            let result = fetch_procs(&target).await;
            let took = start.elapsed();
            let errored = result.is_err();
            if tx
                .send(HostMsg {
                    host_idx,
                    msg: CollectorMsg::Procs { result, took },
                })
                .await
                .is_err()
            {
                break;
            }
            interval = next_interval(interval, base, errored);
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = refresh.notified() => {}
            }
        }
    })
}

fn spawn_db(
    host_idx: usize,
    target: DbTarget,
    tx: mpsc::Sender<HostMsg>,
    refresh: Arc<Notify>,
    base: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = base;
        let mut conn: Option<DbBackend> = None;
        loop {
            let start = Instant::now();

            if conn.is_none() {
                match DbBackend::connect(&target).await {
                    Ok(c) => conn = Some(c),
                    Err(e) => {
                        let took = start.elapsed();
                        let result: Result<DbStats> = Err(e);
                        if tx
                            .send(HostMsg {
                                host_idx,
                                msg: CollectorMsg::Db { result, took },
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                        interval = next_interval(interval, base, true);
                        tokio::select! {
                            _ = tokio::time::sleep(interval) => {}
                            _ = refresh.notified() => {}
                        }
                        continue;
                    }
                }
            }

            let result = conn.as_mut().unwrap().fetch_stats(target.timeout).await;
            let took = start.elapsed();
            let errored = result.is_err();
            if errored {
                conn = None;
            }
            if tx
                .send(HostMsg {
                    host_idx,
                    msg: CollectorMsg::Db { result, took },
                })
                .await
                .is_err()
            {
                break;
            }
            interval = next_interval(interval, base, errored);
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = refresh.notified() => {}
            }
        }
    })
}

fn spawn_stats(
    host_idx: usize,
    target: ZbxStatsTarget,
    tx: mpsc::Sender<HostMsg>,
    refresh: Arc<Notify>,
    base: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = base;
        loop {
            let start = Instant::now();
            let result = fetch_stats(&target).await;
            let took = start.elapsed();
            let errored = result.is_err();
            if tx
                .send(HostMsg {
                    host_idx,
                    msg: CollectorMsg::Stats { result, took },
                })
                .await
                .is_err()
            {
                break;
            }
            interval = next_interval(interval, base, errored);
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = refresh.notified() => {}
            }
        }
    })
}

fn spawn_sys(
    host_idx: usize,
    target: SshTarget,
    tx: mpsc::Sender<HostMsg>,
    refresh: Arc<Notify>,
    base: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = base;
        loop {
            let start = Instant::now();
            let result = fetch_sys(&target).await;
            let took = start.elapsed();
            let errored = result.is_err();
            if tx
                .send(HostMsg {
                    host_idx,
                    msg: CollectorMsg::Sys { result, took },
                })
                .await
                .is_err()
            {
                break;
            }
            interval = next_interval(interval, base, errored);
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = refresh.notified() => {}
            }
        }
    })
}

// ---- streaming log task (v0.2b) ----

fn spawn_log_stream(
    host_idx: usize,
    target: SshTarget,
    log_path: String,
    initial_lines: usize,
    tx: mpsc::Sender<HostMsg>,
    reconnect: Arc<Notify>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reconnects: u32 = 0;
        let mut backoff = Duration::from_secs(1);

        loop {
            let lines = if reconnects == 0 { initial_lines } else { 0 };
            let result = run_stream(host_idx, &target, &log_path, lines, &tx).await;

            let err_msg = match result {
                Ok(()) => Some("stream ended (EOF)".to_string()),
                Err(e) => Some(format!("{e:#}")),
            };

            reconnects = reconnects.saturating_add(1);
            if tx
                .send(HostMsg {
                    host_idx,
                    msg: CollectorMsg::LogStreamStatus(LogStreamStatus {
                        connected: false,
                        reconnects,
                        last_error: err_msg,
                    }),
                })
                .await
                .is_err()
            {
                break;
            }

            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = reconnect.notified() => {}
            }
            backoff = (backoff.saturating_mul(2)).min(MAX_BACKOFF);
        }
    })
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

async fn run_stream(
    host_idx: usize,
    target: &SshTarget,
    log_path: &str,
    initial_lines: usize,
    tx: &mpsc::Sender<HostMsg>,
) -> Result<()> {
    let remote_cmd = format!("tail -n {} -F -- {} 2>/dev/null", initial_lines, shell_quote(log_path));

    let mut cmd = Command::new("ssh");
    cmd.args(&target.extra_opts);
    cmd.arg(&target.host);
    cmd.arg(&remote_cmd);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn ssh {} (tail -F)", target.host))?;
    let stdout = child
        .stdout
        .take()
        .context("ssh child has no stdout pipe")?;

    tx.send(HostMsg {
        host_idx,
        msg: CollectorMsg::LogStreamStatus(LogStreamStatus {
            connected: true,
            reconnects: 0,
            last_error: None,
        }),
    })
    .await
    .ok();

    let mut reader = BufReader::new(stdout).lines();
    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                let entry = LogLine {
                    level: classify(&line),
                    raw: line,
                };
                if tx
                    .send(HostMsg {
                        host_idx,
                        msg: CollectorMsg::LogStreamLine(entry),
                    })
                    .await
                    .is_err()
                {
                    let _ = child.start_kill();
                    return Ok(());
                }
            }
            Ok(None) => {
                let _ = child.wait().await;
                return Ok(());
            }
            Err(e) => {
                let _ = child.start_kill();
                return Err(anyhow::anyhow!("read stdout: {e}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_on_error_and_caps() {
        let base = Duration::from_secs(2);
        let after = next_interval(base, base, true);
        assert_eq!(after, Duration::from_secs(4));
        let after = next_interval(after, base, true);
        assert_eq!(after, Duration::from_secs(8));
        let after = next_interval(after, base, true);
        assert_eq!(after, Duration::from_secs(16));
        let after = next_interval(after, base, true);
        assert_eq!(after, Duration::from_secs(30));
        let after = next_interval(after, base, true);
        assert_eq!(after, Duration::from_secs(30));
    }

    #[test]
    fn backoff_resets_on_success() {
        let base = Duration::from_secs(2);
        let after = next_interval(Duration::from_secs(16), base, false);
        assert_eq!(after, base);
    }

    #[test]
    fn stale_threshold_works() {
        let mut s = SourceState::default();
        assert!(s.is_stale(Duration::from_secs(1)));
        s.last_ok = Some(Instant::now());
        assert!(!s.is_stale(Duration::from_secs(5)));
    }
}
