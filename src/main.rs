//! ztop — TUI монитор внутреннего состояния zabbix_server по SSH.
//!
//! v0.5a: multi-host. Хосты задаются либо через CLI (single-host),
//! либо через `~/.config/ztop/hosts.toml` (или `--config <path>`).

mod app;
mod collectors;
mod db;
mod dbcreds;
mod diagnose;
mod hosts;
mod probes;
mod record;
mod source;
mod ssh;
mod ui;
mod zbxstats;

use crate::app::{App, HostState, Tab};
use crate::collectors::{runtime_control, RuntimeCmd};
use crate::db::DbTarget;
use crate::hosts::{HostConfig, HostsConfig};
use crate::source::{spawn_collectors, CollectorHandles, CollectorMsg, HostMsg, HostSpawn};
use crate::ssh::SshTarget;
use crate::zbxstats::ZbxStatsTarget;
use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io, path::PathBuf, time::Duration};
use tokio::sync::mpsc;
use tokio::time::interval;

#[derive(Parser, Debug)]
#[command(version, about = "TUI monitor for zabbix_server over SSH", long_about = None)]
struct Cli {
    /// Удалённый хост (single-host shortcut). Игнорируется, если задан --config
    /// или дефолтный конфиг существует.
    #[arg(short = 'H', long, env = "ZTOP_HOST")]
    host: Option<String>,

    /// Путь к hosts.toml. По умолчанию — `~/.config/ztop/hosts.toml`
    /// (если существует).
    #[arg(short = 'C', long, env = "ZTOP_CONFIG")]
    config: Option<PathBuf>,

    #[arg(short = 'l', long, env = "ZTOP_LOG", default_value = "/var/log/zabbix/zabbix_server.log")]
    log: String,

    #[arg(long, env = "ZTOP_SUDO", default_value_t = false)]
    sudo: bool,

    #[arg(short = 't', long, default_value_t = 2)]
    tick: u64,

    #[arg(long, default_value_t = 200)]
    log_lines: usize,

    #[arg(long, env = "ZTOP_STATS_HOST")]
    stats_host: Option<String>,

    #[arg(long, env = "ZTOP_STATS_PORT", default_value_t = 10051)]
    stats_port: u16,

    #[arg(long, env = "ZTOP_NO_STATS", default_value_t = false)]
    no_stats: bool,

    #[arg(long, env = "ZTOP_DB_URL")]
    db_url: Option<String>,

    #[arg(long, env = "ZTOP_DB_INSECURE_TLS", default_value_t = false)]
    db_insecure_tls: bool,

    /// Записывать всю поступающую телеметрию в JSONL-файл для постмортема.
    /// Пример: `--record incident-2026-05-13.jsonl`.
    #[arg(long, env = "ZTOP_RECORD")]
    record: Option<PathBuf>,

    /// Воспроизвести запись (read-only). Хосты, окружение, диагнозы —
    /// всё восстанавливается. Runtime control и SSH-коннекты не делаются.
    #[arg(long, env = "ZTOP_REPLAY", conflicts_with = "record")]
    replay: Option<PathBuf>,

    /// Скорость воспроизведения (1.0 — реальное время, 10.0 — в 10 раз быстрее).
    #[arg(long, default_value_t = 1.0)]
    replay_speed: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Replay-режим: хосты восстанавливаются из header-а записи.
    let replay_header = if let Some(path) = &cli.replay {
        Some(record::read_header(path)?)
    } else {
        None
    };

    // Резолвим список хостов и проб: replay > явный --config > дефолт > CLI single-host.
    let (hosts_cfg, probes_cfg) = if let Some(h) = &replay_header {
        let hcs = h
            .hosts
            .iter()
            .map(|m| hosts::HostConfig {
                name: m.name.clone(),
                ssh: m.ssh.clone(),
                log: m.log_path.clone(),
                sudo: false,
                stats_host: None,
                stats_port: 10051,
                no_stats: !m.stats_enabled,
                db_url: if m.db_enabled {
                    Some(String::new())
                } else {
                    None
                },
                db_insecure_tls: false,
            })
            .collect();
        (hcs, Vec::<probes::ProbeConfig>::new())
    } else {
        resolve_full_config(&cli)?
    };

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Строим HostState и HostSpawn по конфигу.
    let mut states = Vec::with_capacity(hosts_cfg.len());
    let mut spawns = Vec::with_capacity(hosts_cfg.len());
    for (idx, h) in hosts_cfg.iter().enumerate() {
        let stats_enabled = !h.no_stats;
        let db_target = h.db_url.as_ref().map(|url| {
            let enriched = if url.starts_with("mysql://") {
                dbcreds::enrich_my_url(url)
            } else {
                dbcreds::enrich_pg_url(url)
            };
            DbTarget::new(enriched).with_insecure_tls(h.db_insecure_tls)
        });
        let stats_target = if stats_enabled {
            let stats_host = h
                .stats_host
                .clone()
                .unwrap_or_else(|| strip_user(&h.ssh).to_string());
            Some(ZbxStatsTarget::new(stats_host, h.stats_port))
        } else {
            None
        };

        states.push(HostState::new(
            h.name.clone(),
            h.ssh.clone(),
            h.log.clone(),
            h.sudo,
            stats_enabled,
            db_target.is_some(),
        ));
        spawns.push(HostSpawn {
            host_idx: idx,
            ssh: SshTarget::new(h.ssh.clone()),
            log_path: h.log.clone(),
            log_lines: cli.log_lines,
            stats: stats_target,
            db: db_target,
        });
    }

    let mut app = App::new(states);
    app.is_replay = cli.replay.is_some();
    // v0.7: инициализируем probes state
    app.probes = probes_cfg
        .iter()
        .map(probes::ProbeState::from_config)
        .collect();

    let res = run_app(
        &mut terminal,
        &mut app,
        spawns,
        probes_cfg,
        cli.tick,
        cli.record.clone(),
        cli.replay.clone(),
        cli.replay_speed,
    )
    .await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(e) = res {
        eprintln!("ztop exited with error: {e:#}");
        std::process::exit(1);
    }
    Ok(())
}

/// Резолвит финальный список (`HostConfig`, `ProbeConfig`) с учётом приоритета:
/// --config (явный) > дефолтный hosts.toml > CLI single-host.
fn resolve_full_config(
    cli: &Cli,
) -> Result<(Vec<HostConfig>, Vec<probes::ProbeConfig>)> {
    // Явный --config.
    if let Some(path) = &cli.config {
        let cfg = HostsConfig::load_from(path)?;
        return Ok((cfg.hosts, cfg.probes));
    }
    // Дефолтный путь.
    if let Some(default) = HostsConfig::default_path() {
        if default.exists() {
            let cfg = HostsConfig::load_from(&default)
                .with_context(|| format!("load default config {}", default.display()))?;
            return Ok((cfg.hosts, cfg.probes));
        }
    }
    // Fallback: single-host из CLI (без проб).
    let host = cli
        .host
        .clone()
        .context("no hosts configured: pass --host or create ~/.config/ztop/hosts.toml")?;
    Ok((
        vec![HostConfig {
            name: strip_user(&host).to_string(),
            ssh: host,
            log: cli.log.clone(),
            sudo: cli.sudo,
            stats_host: cli.stats_host.clone(),
            stats_port: cli.stats_port,
            no_stats: cli.no_stats,
            db_url: cli.db_url.clone(),
            db_insecure_tls: cli.db_insecure_tls,
        }],
        vec![],
    ))
}

fn strip_user(ssh: &str) -> &str {
    ssh.split_once('@').map(|(_, h)| h).unwrap_or(ssh)
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    spawns: Vec<HostSpawn>,
    probes_cfg: Vec<probes::ProbeConfig>,
    tick_sec: u64,
    record_path: Option<PathBuf>,
    replay_path: Option<PathBuf>,
    replay_speed: f64,
) -> Result<()> {
    let base_interval = Duration::from_secs(tick_sec.max(1));
    let (tx, mut rx) = mpsc::channel::<HostMsg>(512);
    // v0.7: отдельный канал для проб (они глобальные, не привязаны к host).
    let (probe_tx, mut probe_rx) = mpsc::channel::<probes::ProbeMsg>(128);
    let _probe_handles = probes::spawn_probes(probes_cfg, probe_tx);

    // Recorder (если --record): открываем файл, пишем header с хостами.
    let mut recorder: Option<record::Recorder> = if let Some(path) = &record_path {
        let metas: Vec<record::HostMeta> = app
            .hosts
            .iter()
            .map(|h| record::HostMeta {
                name: h.name.clone(),
                ssh: h.ssh_host.clone(),
                log_path: h.log_path.clone(),
                stats_enabled: h.stats_enabled,
                db_enabled: h.db_enabled,
            })
            .collect();
        Some(record::Recorder::create(path, metas)?)
    } else {
        None
    };

    // Замена для spawn_collectors: либо живые коллекторы, либо replay-loop.
    let handles: Option<CollectorHandles> = if let Some(path) = replay_path {
        // Replay-режим (v0.6.1): загружаем всю запись в память, чтобы
        // поддерживать seek в обе стороны. Команды управления идут через
        // unbounded-канал из event-loop.
        let (_header, events) = record::load_recording(&path)?;
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        app.replay_cmd_tx = Some(cmd_tx);
        let progress = app.replay_progress.clone();
        let paused_flag = app.replay_paused_flag.clone();
        let tx_replay = tx.clone();
        tokio::spawn(async move {
            let _ = record::replay_loop(events, tx_replay, cmd_rx, replay_speed, progress, paused_flag).await;
        });
        None
    } else {
        Some(spawn_collectors(spawns, base_interval, tx))
    };

    let mut events = EventStream::new();
    let mut ui_ticker = interval(Duration::from_millis(500));
    ui_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    terminal.draw(|f| ui::draw(f, app))?;

    let exit_reason = loop {
        tokio::select! {
            biased;
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        // В replay-режиме refresh/reconnect не имеют смысла —
                        // даём заглушки-Notify, которые никого не разбудят.
                        let dummy_refresh = std::sync::Arc::new(tokio::sync::Notify::new());
                        let dummy_log_reconnect = std::sync::Arc::new(tokio::sync::Notify::new());
                        let (refresh, log_reconnect) = match &handles {
                            Some(h) => (&h.refresh, &h.log_reconnect),
                            None => (&dummy_refresh, &dummy_log_reconnect),
                        };
                        if handle_key(app, refresh, log_reconnect, key).await? {
                            break "user quit";
                        }
                        terminal.draw(|f| ui::draw(f, app))?;
                    }
                    Some(Ok(Event::Resize(_, _))) => {
                        terminal.draw(|f| ui::draw(f, app))?;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break "input stream closed",
                }
            }
            Some(probe_msg) = probe_rx.recv() => {
                app.apply_probe(probe_msg);
                terminal.draw(|f| ui::draw(f, app))?;
            }
            Some(host_msg) = rx.recv() => {
                // Запись (если включена) — до apply_msg, потому что HostMsg
                // потребляется ниже. Recorder клонирует данные внутри.
                if let Some(rec) = &mut recorder {
                    if !matches!(host_msg.msg, CollectorMsg::Reset) {
                        rec.write(&host_msg);
                        app.recorded_events = rec.events_written;
                    }
                }
                if !app.paused {
                    if let Some(host) = app.hosts.get_mut(host_msg.host_idx) {
                        host.apply_msg(host_msg.msg);
                        app.last_refresh = Some(chrono::Local::now());
                    }
                }
                terminal.draw(|f| ui::draw(f, app))?;
            }
            _ = ui_ticker.tick() => {
                app.tick_decay();
                terminal.draw(|f| ui::draw(f, app))?;
            }
        }
    };

    let _ = exit_reason;
    if let Some(h) = handles {
        h.shutdown();
    }
    // recorder.drop() флашит файл автоматически
    drop(recorder);
    Ok(())
}

async fn handle_key(
    app: &mut App,
    refresh: &std::sync::Arc<tokio::sync::Notify>,
    log_reconnect: &std::sync::Arc<tokio::sync::Notify>,
    key: KeyEvent,
) -> Result<bool> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Ok(true);
    }

    // v0.5c: модал-выборщик хостов перехватывает ввод до всего остального.
    if app.show_host_picker {
        let n_matches = app.host_picker_matches().len();
        match key.code {
            KeyCode::Esc => {
                app.show_host_picker = false;
                app.host_picker_query.clear();
            }
            KeyCode::Enter => {
                app.host_picker_apply();
            }
            KeyCode::Up => {
                if app.host_picker_cursor > 0 {
                    app.host_picker_cursor -= 1;
                }
            }
            KeyCode::Down => {
                if app.host_picker_cursor + 1 < n_matches {
                    app.host_picker_cursor += 1;
                }
            }
            KeyCode::Backspace => {
                app.host_picker_query.pop();
                app.host_picker_cursor = 0;
            }
            KeyCode::Char(c) => {
                app.host_picker_query.push(c);
                app.host_picker_cursor = 0;
            }
            _ => {}
        }
        return Ok(false);
    }
    // Ctrl-G — открыть модал.
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('g')) {
        app.open_host_picker();
        return Ok(false);
    }

    // v0.6.1: команды управления replay-сессией.
    if app.is_replay {
        if let Some(tx) = &app.replay_cmd_tx {
            use crate::record::ReplayCmd;
            let cmd = match key.code {
                KeyCode::Char(' ') => Some(ReplayCmd::TogglePause),
                KeyCode::Char('n') if app.replay_paused_flag.load(std::sync::atomic::Ordering::Relaxed) => {
                    Some(ReplayCmd::Step)
                }
                KeyCode::Char('>') | KeyCode::Char('.') => {
                    Some(ReplayCmd::SeekForward(std::time::Duration::from_secs(60)))
                }
                KeyCode::Char('<') | KeyCode::Char(',') => {
                    Some(ReplayCmd::SeekBackward(std::time::Duration::from_secs(60)))
                }
                _ => None,
            };
            if let Some(c) = cmd {
                let _ = tx.send(c);
                return Ok(false);
            }
        }
    }

    // Ctrl-N / Ctrl-P: переключение фокуса между хостами (на любом табе).
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('n') => {
                app.next_host();
                return Ok(false);
            }
            KeyCode::Char('p') => {
                app.prev_host();
                return Ok(false);
            }
            _ => {}
        }
    }

    if app.editing_filter {
        match key.code {
            KeyCode::Esc => {
                app.editing_filter = false;
                app.log_filter.clear();
            }
            KeyCode::Enter => app.editing_filter = false,
            KeyCode::Backspace => {
                app.log_filter.pop();
            }
            KeyCode::Char(c) => app.log_filter.push(c),
            _ => {}
        }
        return Ok(false);
    }

    if app.show_runtime_menu {
        match key.code {
            KeyCode::Esc | KeyCode::Char('R') | KeyCode::Char('q') => {
                app.show_runtime_menu = false;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if app.runtime_cursor > 0 {
                    app.runtime_cursor -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if app.runtime_cursor + 1 < app.runtime_choices.len() {
                    app.runtime_cursor += 1;
                }
            }
            KeyCode::Enter => {
                let cmd = app.runtime_choices[app.runtime_cursor];
                execute_runtime(app, cmd).await;
                app.show_runtime_menu = false;
            }
            _ => {}
        }
        return Ok(false);
    }

    // Стрелки/Enter в Overview табе — навигация по хостам.
    if app.tab == Tab::Overview {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                app.overview_cursor_up();
                return Ok(false);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.overview_cursor_down();
                return Ok(false);
            }
            KeyCode::Enter => {
                app.overview_select();
                app.tab = Tab::Processes;
                return Ok(false);
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Tab => app.next_tab(),
        KeyCode::Char('0') => {
            // При входе на Overview синхронизируем курсор с focused_host.
            app.overview_cursor = app.focused_host;
            app.tab = Tab::Overview;
        }
        KeyCode::Char('1') => app.tab = Tab::Processes,
        KeyCode::Char('2') => app.tab = Tab::Graphs,
        KeyCode::Char('3') => app.tab = Tab::Logs,
        KeyCode::Char('4') => app.tab = Tab::Internals,
        KeyCode::Char('5') => app.tab = Tab::Database,
        KeyCode::Char('6') => app.tab = Tab::Probes,
        KeyCode::Char('p') => app.paused = !app.paused,
        KeyCode::Char('r') => refresh.notify_waiters(),
        KeyCode::Char('L') => log_reconnect.notify_waiters(),
        KeyCode::Char('R') => {
            app.show_runtime_menu = true;
            app.runtime_cursor = 0;
        }
        KeyCode::Char('+') | KeyCode::Char('=') => {
            execute_runtime(app, RuntimeCmd::LogLevelIncrease).await
        }
        KeyCode::Char('-') | KeyCode::Char('_') => {
            execute_runtime(app, RuntimeCmd::LogLevelDecrease).await
        }
        KeyCode::Char('c') => execute_runtime(app, RuntimeCmd::ConfigCacheReload).await,
        KeyCode::Char('h') => execute_runtime(app, RuntimeCmd::HousekeeperExecute).await,
        KeyCode::Char('d') => execute_runtime(app, RuntimeCmd::Diaginfo).await,
        KeyCode::Char('/') => {
            app.editing_filter = true;
            app.tab = Tab::Logs;
        }
        _ => {}
    }
    Ok(false)
}

async fn execute_runtime(app: &mut App, cmd: RuntimeCmd) {
    if app.is_replay {
        app.set_toast("runtime control disabled in --replay mode", true);
        return;
    }
    let target = SshTarget::new(app.focused().ssh_host.clone());
    let use_sudo = app.focused().use_sudo;
    let host_name = app.focused().name.clone();
    match runtime_control(&target, use_sudo, cmd).await {
        Ok(out) => {
            let snippet = out.lines().next().unwrap_or("").to_string();
            app.set_toast(format!("[{}] {}: {}", host_name, cmd.label(), snippet), false);
        }
        Err(e) => {
            app.set_toast(format!("[{}] {} failed: {}", host_name, cmd.label(), e), true);
        }
    }
}
