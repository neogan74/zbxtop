//! SSH transport: invokes the system `ssh` via tokio.
//!
//! Why system `ssh` rather than `russh`/`ssh2`:
//! - works for free with ~/.ssh/config, ControlMaster, keys, agent, jump hosts;
//! - this is critical for a prototype — we want to reach any existing infrastructure;
//! - for v2 it makes sense to switch to a single long-lived session via `russh` to keep
//!   a tail -F stream without paying for a TCP handshake on every poll.

use anyhow::{anyhow, Context, Result};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

#[derive(Clone, Debug)]
pub struct SshTarget {
    /// Hostname or alias from ~/.ssh/config. May be "user@host".
    pub host: String,
    /// Additional options (-o ...). Defaults set ControlMaster for multiplexing.
    pub extra_opts: Vec<String>,
    /// Global timeout per command.
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
    // ControlMaster auto+ControlPersist greatly reduce latency for repeated commands.
    // Socket path is under ~/.ssh with a unique name per host.
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

/// Execute a shell command on the remote host and return stdout as a string.
/// stderr is appended to the error message if the exit code is non-zero.
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
