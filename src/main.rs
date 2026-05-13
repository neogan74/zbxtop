//! ztop — TUI монитор внутреннего состояния zabbix_server по SSH.
//!
//! Запуск:  ztop --host zbx-prod-01 --log /var/log/zabbix/zabbix_server.log [--sudo]

mod app;
mod collectors;
mod ssh;
mod ui;

use crate::app::{App, Tab};
use crate::collectors::{
    aggregate_roles, fetch_log_tail, fetch_procs, fetch_sys, runtime_control, RuntimeCmd,
};
use crate::ssh::SshTarget;
use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io, time::Duration};
use tokio::time::interval;

#[derive(Parser, Debug)]
#[command(version, about = "TUI monitor for zabbix_server over SSH", long_about = None)]
struct Cli {
    /// Удалённый хост (или alias из ~/.ssh/config). Может быть user@host.
    #[arg(short = 'H', long, env = "ZTOP_HOST")]
    host: String,

    /// Полный путь к zabbix_server.log на удалённом хосте.
    #[arg(short = 'l', long, env = "ZTOP_LOG", default_value = "/var/log/zabbix/zabbix_server.log")]
    log: String,

    /// Использовать `sudo -n` для runtime control команд.
    #[arg(long, env = "ZTOP_SUDO", default_value_t = false)]
    sudo: bool,

    /// Интервал опроса в секундах.
    #[arg(short = 't', long, default_value_t = 2)]
    tick: u64,

    /// Сколько последних строк лога подтягивать.
    #[arg(long, default_value_t = 200)]
    log_lines: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let target = SshTarget::new(cli.host.clone());

    // Терминал
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(cli.host.clone(), cli.log.clone(), cli.sudo);
    let res = run_app(&mut terminal, &mut app, target, cli.tick, cli.log_lines).await;

    // Откат терминала, даже если упало
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

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    target: SshTarget,
    tick_sec: u64,
    log_lines: usize,
) -> Result<()> {
    let mut events = EventStream::new();
    let mut ticker = interval(Duration::from_secs(tick_sec.max(1)));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Первый рефреш сразу
    refresh(app, &target, log_lines).await;
    terminal.draw(|f| ui::draw(f, app))?;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                app.tick_decay();
                if !app.paused {
                    refresh(app, &target, log_lines).await;
                }
                terminal.draw(|f| ui::draw(f, app))?;
            }
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        if handle_key(app, &target, key).await? {
                            break;
                        }
                        terminal.draw(|f| ui::draw(f, app))?;
                    }
                    Some(Ok(Event::Resize(_, _))) => {
                        terminal.draw(|f| ui::draw(f, app))?;
                    }
                    Some(Err(_)) | None => break,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

async fn refresh(app: &mut App, target: &SshTarget, log_lines: usize) {
    // Параллельно дёргаем три коллектора. Каждый ssh — отдельный коннект, но с
    // ControlMaster они мультиплексируются через один сокет — это дёшево.
    let (procs_r, sys_r, logs_r) = tokio::join!(
        fetch_procs(target),
        fetch_sys(target),
        fetch_log_tail(target, &app.log_path, log_lines),
    );

    let mut errs = Vec::new();

    match procs_r {
        Ok(procs) => {
            app.roles = aggregate_roles(&procs);
            app.procs = procs;
        }
        Err(e) => errs.push(format!("ps: {e}")),
    }
    match sys_r {
        Ok(s) => app.sys = s,
        Err(e) => errs.push(format!("sys: {e}")),
    }
    match logs_r {
        Ok(l) => app.logs = l,
        Err(e) => errs.push(format!("log: {e}")),
    }

    app.history.push(
        app.cpu_sum(),
        app.sys.mem_used_pct() as f64,
        app.sys.load1 as f64,
    );
    app.last_refresh = Some(chrono::Local::now());
    app.last_error = if errs.is_empty() {
        None
    } else {
        Some(errs.join(" | "))
    };
}

/// Возвращает true, если пора выходить.
async fn handle_key(app: &mut App, target: &SshTarget, key: KeyEvent) -> Result<bool> {
    // Ctrl-C — всегда выход.
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Ok(true);
    }

    // Режим редактирования фильтра лога перехватывает ввод.
    if app.editing_filter {
        match key.code {
            KeyCode::Esc => {
                app.editing_filter = false;
                app.log_filter.clear();
            }
            KeyCode::Enter => {
                app.editing_filter = false;
            }
            KeyCode::Backspace => {
                app.log_filter.pop();
            }
            KeyCode::Char(c) => {
                app.log_filter.push(c);
            }
            _ => {}
        }
        return Ok(false);
    }

    // Модальное меню runtime control.
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
                execute_runtime(app, target, cmd).await;
                app.show_runtime_menu = false;
            }
            _ => {}
        }
        return Ok(false);
    }

    // Глобальные клавиши.
    match key.code {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Tab => app.next_tab(),
        KeyCode::Char('1') => app.tab = Tab::Processes,
        KeyCode::Char('2') => app.tab = Tab::Graphs,
        KeyCode::Char('3') => app.tab = Tab::Logs,
        KeyCode::Char('p') => app.paused = !app.paused,
        KeyCode::Char('r') => refresh(app, target, 200).await,
        KeyCode::Char('R') => {
            app.show_runtime_menu = true;
            app.runtime_cursor = 0;
        }
        KeyCode::Char('+') | KeyCode::Char('=') => {
            execute_runtime(app, target, RuntimeCmd::LogLevelIncrease).await;
        }
        KeyCode::Char('-') | KeyCode::Char('_') => {
            execute_runtime(app, target, RuntimeCmd::LogLevelDecrease).await;
        }
        KeyCode::Char('c') => execute_runtime(app, target, RuntimeCmd::ConfigCacheReload).await,
        KeyCode::Char('h') => execute_runtime(app, target, RuntimeCmd::HousekeeperExecute).await,
        KeyCode::Char('d') => execute_runtime(app, target, RuntimeCmd::Diaginfo).await,
        KeyCode::Char('/') => {
            app.editing_filter = true;
            app.tab = Tab::Logs;
        }
        _ => {}
    }
    Ok(false)
}

async fn execute_runtime(app: &mut App, target: &SshTarget, cmd: RuntimeCmd) {
    match runtime_control(target, app.use_sudo, cmd).await {
        Ok(out) => {
            let snippet = out.lines().next().unwrap_or("").to_string();
            app.set_toast(format!("{}: {}", cmd.label(), snippet), false);
        }
        Err(e) => {
            app.set_toast(format!("{} failed: {}", cmd.label(), e), true);
        }
    }
}
