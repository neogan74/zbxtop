//! v0.6 — запись и воспроизведение телеметрии для постмортема.
//!
//! Формат: JSON Lines. Первая строка — `HeaderLine` с метаданными хостов
//! (имя, ssh, log_path) и версией протокола. Каждая последующая строка —
//! `EventLine` с timestamp-ом, host_idx и сериализованным сообщением.
//!
//! Запись (`--record path`): после каждого `HostMsg` event-loop вызывает
//! `Recorder::write`, JSON-строка кладётся в BufWriter. Файл закрывается
//! на graceful shutdown через Drop.
//!
//! Воспроизведение (`--replay path`): отдельная задача читает JSONL,
//! шлёт `HostMsg` в тот же mpsc-канал, который обычно использует
//! `spawn_collectors`. Между событиями делает sleep до оригинального
//! timestamp-а — UI выглядит как при «живом» прогоне.
//!
//! Особо ценно вместе с **diagnose** (v0.4b): replay инцидента сразу
//! показывает, какие диагнозы загорелись и когда. Постмортем превращается
//! из чтения логов в «промотать вперёд и посмотреть».

use crate::collectors::{LogLine, SysStats, ZbxProc};
use crate::db::DbStats;
use crate::source::{CollectorMsg, HostMsg, LogStreamStatus};
use crate::zbxstats::ZabbixStats;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const FORMAT_VERSION: &str = "0.6";

// ---------- header ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Line {
    Header(HeaderLine),
    Event(EventLine),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderLine {
    pub version: String,
    pub started_at: DateTime<Utc>,
    pub hosts: Vec<HostMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostMeta {
    pub name: String,
    pub ssh: String,
    pub log_path: String,
    pub stats_enabled: bool,
    pub db_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLine {
    pub ts: DateTime<Utc>,
    pub host_idx: usize,
    pub msg: RecordedMsg,
}

// ---------- serializable mirror of CollectorMsg ----------
//
// CollectorMsg несёт `Result<T, anyhow::Error>` — Error не сериализуется.
// Здесь все Err преобразованы в String (через `format!("{e:#}")`).

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RecordedMsg {
    Procs {
        result: Result<Vec<ZbxProc>, String>,
        took_ms: u64,
    },
    Sys {
        result: Result<SysStats, String>,
        took_ms: u64,
    },
    Stats {
        result: Result<ZabbixStats, String>,
        took_ms: u64,
    },
    Db {
        result: Result<DbStats, String>,
        took_ms: u64,
    },
    LogStreamLine(LogLine),
    LogStreamStatus(LogStreamStatus),
}

impl RecordedMsg {
    pub fn from_collector(msg: &CollectorMsg) -> Self {
        match msg {
            CollectorMsg::Procs { result, took } => Self::Procs {
                result: result
                    .as_ref()
                    .map(|v| v.clone())
                    .map_err(|e| format!("{e:#}")),
                took_ms: took.as_millis() as u64,
            },
            CollectorMsg::Sys { result, took } => Self::Sys {
                result: result
                    .as_ref()
                    .map(|v| v.clone())
                    .map_err(|e| format!("{e:#}")),
                took_ms: took.as_millis() as u64,
            },
            CollectorMsg::Stats { result, took } => Self::Stats {
                result: result
                    .as_ref()
                    .map(|v| v.clone())
                    .map_err(|e| format!("{e:#}")),
                took_ms: took.as_millis() as u64,
            },
            CollectorMsg::Db { result, took } => Self::Db {
                result: result
                    .as_ref()
                    .map(|v| v.clone())
                    .map_err(|e| format!("{e:#}")),
                took_ms: took.as_millis() as u64,
            },
            CollectorMsg::LogStreamLine(l) => Self::LogStreamLine(l.clone()),
            CollectorMsg::LogStreamStatus(s) => Self::LogStreamStatus(s.clone()),
            // Reset не записываем — это синтетический сигнал, существует
            // только in-memory между replay-loop и event-loop.
            CollectorMsg::Reset => Self::LogStreamStatus(crate::source::LogStreamStatus::default()),
        }
    }

    pub fn into_collector(self) -> CollectorMsg {
        match self {
            Self::Procs { result, took_ms } => CollectorMsg::Procs {
                result: result.map_err(|s| anyhow!(s)),
                took: Duration::from_millis(took_ms),
            },
            Self::Sys { result, took_ms } => CollectorMsg::Sys {
                result: result.map_err(|s| anyhow!(s)),
                took: Duration::from_millis(took_ms),
            },
            Self::Stats { result, took_ms } => CollectorMsg::Stats {
                result: result.map_err(|s| anyhow!(s)),
                took: Duration::from_millis(took_ms),
            },
            Self::Db { result, took_ms } => CollectorMsg::Db {
                result: result.map_err(|s| anyhow!(s)),
                took: Duration::from_millis(took_ms),
            },
            Self::LogStreamLine(l) => CollectorMsg::LogStreamLine(l),
            Self::LogStreamStatus(s) => CollectorMsg::LogStreamStatus(s),
        }
    }
}

// ---------- Recorder ----------

pub struct Recorder {
    writer: BufWriter<File>,
    /// Сколько событий записано (для UI-индикатора).
    pub events_written: u64,
}

impl Recorder {
    pub fn create(path: &Path, hosts: Vec<HostMeta>) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .with_context(|| format!("open recording file {}", path.display()))?;
        let mut writer = BufWriter::new(file);

        let header = Line::Header(HeaderLine {
            version: FORMAT_VERSION.to_string(),
            started_at: Utc::now(),
            hosts,
        });
        serde_json::to_writer(&mut writer, &header)?;
        writer.write_all(b"\n")?;
        writer.flush()?;

        Ok(Self {
            writer,
            events_written: 0,
        })
    }

    pub fn write(&mut self, host_msg: &HostMsg) {
        let line = Line::Event(EventLine {
            ts: Utc::now(),
            host_idx: host_msg.host_idx,
            msg: RecordedMsg::from_collector(&host_msg.msg),
        });
        // Игнорируем ошибки записи: если файл недоступен, нет смысла валить UI.
        // В будущем — toast «recording paused: <err>».
        if serde_json::to_writer(&mut self.writer, &line).is_ok() {
            let _ = self.writer.write_all(b"\n");
            self.events_written = self.events_written.saturating_add(1);
            // Не flush — BufWriter сам сбросит при закрытии или переполнении.
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.writer.flush();
    }
}

// ---------- Replay ----------

/// Прочитать только header — для построения списка хостов до старта event-loop.
pub fn read_header(path: &Path) -> Result<HeaderLine> {
    let file = File::open(path)
        .with_context(|| format!("open replay file {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let first = first.trim();
    if first.is_empty() {
        return Err(anyhow!("replay file is empty"));
    }
    let line: Line = serde_json::from_str(first)
        .with_context(|| "parse first line as header")?;
    match line {
        Line::Header(h) => {
            if h.version != FORMAT_VERSION {
                eprintln!(
                    "warning: replay format version {} differs from current {}",
                    h.version, FORMAT_VERSION
                );
            }
            Ok(h)
        }
        Line::Event(_) => Err(anyhow!("first line is event, not header")),
    }
}

/// v0.6.1 — команды управления replay-сессией.
#[derive(Debug, Clone, Copy)]
pub enum ReplayCmd {
    TogglePause,
    /// Шаг — один event вперёд (имеет смысл только в paused-состоянии).
    Step,
    /// Прыжок вперёд на заданный интервал. Не сбрасывает state — старые
    /// данные остаются, новые приходят с нового cursor-а.
    SeekForward(Duration),
    /// Прыжок назад. **Сбрасывает HostState всех хостов** (через `Reset`),
    /// иначе UI показывал бы данные из будущего относительно курсора.
    SeekBackward(Duration),
}

/// Загрузить всю запись в память: header + Vec<EventLine>. Для часовой
/// записи ~3 хоста ≈ 50-200 MB; для постмортем-инструмента приемлемо.
/// Lazy-streaming (как было в v0.6) даёт seek-only-forward, что бесполезно.
pub fn load_recording(path: &Path) -> Result<(HeaderLine, Vec<EventLine>)> {
    let file = File::open(path)
        .with_context(|| format!("open replay file {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    let first = lines
        .next()
        .ok_or_else(|| anyhow!("replay file is empty"))??;
    let header = match serde_json::from_str::<Line>(&first)? {
        Line::Header(h) => h,
        Line::Event(_) => return Err(anyhow!("first line is event, not header")),
    };

    let mut events = Vec::new();
    for line in lines {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(Line::Event(e)) = serde_json::from_str::<Line>(&line) {
            events.push(e);
        }
    }
    Ok((header, events))
}

/// Replay-loop с поддержкой команд (pause/step/seek) и индикатором прогресса.
/// Прогресс публикуется в `progress` (0..=1000, для 0.1% точности).
pub async fn replay_loop(
    events: Vec<EventLine>,
    tx: mpsc::Sender<HostMsg>,
    mut cmd_rx: mpsc::UnboundedReceiver<ReplayCmd>,
    speed: f64,
    progress: Arc<AtomicU32>,
    paused_flag: Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let total = events.len();

    let mut cursor: usize = 0;
    let mut paused = false;
    let mut anchor_real = Instant::now();
    let mut anchor_replay = events[0].ts;

    while cursor < total {
        let pct = ((cursor as f32 / total as f32) * 1000.0) as u32;
        progress.store(pct, Ordering::Relaxed);
        paused_flag.store(paused, Ordering::Relaxed);

        let event_ts = events[cursor].ts;
        let offset_ms = (event_ts - anchor_replay).num_milliseconds().max(0) as u64;
        let scaled_ms = ((offset_ms as f64) / speed.max(0.001)) as u64;
        let target = anchor_real + Duration::from_millis(scaled_ms);
        let now = Instant::now();
        let sleep_dur = if target > now {
            target - now
        } else {
            Duration::ZERO
        };

        tokio::select! {
            biased;
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    ReplayCmd::TogglePause => {
                        paused = !paused;
                        // При unpause «переякориваемся» от текущего момента,
                        // чтобы не догонять накопившийся sleep.
                        if !paused {
                            anchor_real = Instant::now();
                            anchor_replay = events[cursor].ts;
                        }
                    }
                    ReplayCmd::Step => {
                        // Шаг работает в любом состоянии, но особенно нужен
                        // в paused — продвигает на один event с немедленной отправкой.
                        let event = events[cursor].clone();
                        let _ = tx
                            .send(HostMsg {
                                host_idx: event.host_idx,
                                msg: event.msg.into_collector(),
                            })
                            .await;
                        cursor += 1;
                    }
                    ReplayCmd::SeekForward(d) => {
                        let target_ts = event_ts
                            + chrono::Duration::from_std(d).unwrap_or_default();
                        cursor = events
                            .partition_point(|e| e.ts < target_ts)
                            .min(total);
                        anchor_real = Instant::now();
                        anchor_replay = events
                            .get(cursor)
                            .map(|e| e.ts)
                            .unwrap_or(anchor_replay);
                    }
                    ReplayCmd::SeekBackward(d) => {
                        let target_ts = event_ts
                            - chrono::Duration::from_std(d).unwrap_or_default();
                        let new_cursor = events.partition_point(|e| e.ts < target_ts);

                        // Reset для каждого хоста, что был в событиях до new_cursor.
                        // Иначе UI показывает данные из «будущего» относительно курсора.
                        let host_idxs: HashSet<usize> = events
                            .iter()
                            .take(new_cursor.max(cursor))
                            .map(|e| e.host_idx)
                            .collect();
                        for idx in host_idxs {
                            let _ = tx
                                .send(HostMsg {
                                    host_idx: idx,
                                    msg: CollectorMsg::Reset,
                                })
                                .await;
                        }

                        cursor = new_cursor;
                        anchor_real = Instant::now();
                        anchor_replay = events
                            .get(cursor)
                            .map(|e| e.ts)
                            .unwrap_or(anchor_replay);
                    }
                }
            }
            _ = tokio::time::sleep(sleep_dur), if !paused => {
                let event = events[cursor].clone();
                if tx
                    .send(HostMsg {
                        host_idx: event.host_idx,
                        msg: event.msg.into_collector(),
                    })
                    .await
                    .is_err()
                {
                    break;
                }
                cursor += 1;
            }
        }
    }
    progress.store(1000, Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_logline() {
        let m = CollectorMsg::LogStreamLine(LogLine {
            raw: "hello".into(),
            level: crate::collectors::LogLevel::Info,
        });
        let rec = RecordedMsg::from_collector(&m);
        let back = rec.into_collector();
        match back {
            CollectorMsg::LogStreamLine(l) => assert_eq!(l.raw, "hello"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_error() {
        let m = CollectorMsg::Sys {
            result: Err(anyhow!("ssh timeout")),
            took: Duration::from_millis(5000),
        };
        let rec = RecordedMsg::from_collector(&m);
        let back = rec.into_collector();
        match back {
            CollectorMsg::Sys { result, took } => {
                assert!(result.is_err());
                assert_eq!(took.as_millis(), 5000);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn header_serializes() {
        let h = HeaderLine {
            version: FORMAT_VERSION.into(),
            started_at: Utc::now(),
            hosts: vec![HostMeta {
                name: "prod".into(),
                ssh: "zbx-prod".into(),
                log_path: "/log".into(),
                stats_enabled: true,
                db_enabled: false,
            }],
        };
        let s = serde_json::to_string(&Line::Header(h.clone())).unwrap();
        assert!(s.contains("header"));
        assert!(s.contains("prod"));
    }
}
