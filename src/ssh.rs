//! SSH transport: вызывает системный `ssh` через tokio.
//!
//! Почему именно system `ssh`, а не `russh`/`ssh2`:
//! - бесплатно работает с ~/.ssh/config, ControlMaster, ключами, агентом, jump-хостами;
//! - для прототипа это критично — мы хотим попасть в любую существующую инфраструктуру;
//! - на v2 имеет смысл заменить на одну долгую сессию через `russh`, чтобы держать
//!   tail -F стрим и не платить за TCP-handshake на каждый poll.

use anyhow::{anyhow, Context, Result};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

#[derive(Clone, Debug)]
pub struct SshTarget {
    /// Имя хоста или alias из ~/.ssh/config. Может быть "user@host".
    pub host: String,
    /// Дополнительные опции (-o ...). По умолчанию задаём ControlMaster для мультиплексирования.
    pub extra_opts: Vec<String>,
    /// Глобальный таймаут на одну команду.
    pub timeout: Duration,
}

impl SshTarget {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            extra_opts: default_opts(),
            timeout: Duration::from_secs(10),
        }
    }
}

fn default_opts() -> Vec<String> {
    // ControlMaster auto+ControlPersist резко сокращают latency повторных команд.
    // Путь к сокету — в /tmp с уникальным именем по хосту.
    vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=5".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ControlMaster=auto".into(),
        "-o".into(),
        "ControlPath=~/.ssh/ztop-ctl-%r@%h:%p".into(),
        "-o".into(),
        "ControlPersist=60s".into(),
    ]
}

/// Выполнить shell-команду на удалённом хосте и вернуть stdout как строку.
/// stderr добавляется в текст ошибки, если код возврата != 0.
pub async fn run(target: &SshTarget, remote_cmd: &str) -> Result<String> {
    let mut cmd = Command::new("ssh");
    cmd.args(&target.extra_opts);
    cmd.arg(&target.host);
    cmd.arg(remote_cmd);
    cmd.kill_on_drop(true);

    let fut = cmd.output();
    let output = timeout(target.timeout, fut)
        .await
        .with_context(|| format!("ssh {} timed out", target.host))?
        .with_context(|| format!("failed to spawn ssh to {}", target.host))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "ssh {} `{}` exit={:?}: {}",
            target.host,
            remote_cmd,
            output.status.code(),
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
