//! v0.7 — synthetic probes (TCP / DNS / PG-SELECT 1).
//!
//! Idea: from the machine running ztop, we periodically fire cheap checks
//! against the network, DB, and DNS. This makes it possible to distinguish
//! "the Zabbix server itself is sick" from "I have network problems from this
//! machine". Probes are global (not tied to the focused host) because they
//! measure the network from ztop, not from the remote host.
//!
//! Config: `[[probe]]` blocks in the same `hosts.toml`. Optional —
//! ztop works without probes (the `probes` section is empty).
//!
//! Probes are opt-in. No automatic scanning or discovery.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::time::{Duration, Instant};
use tokio::net::{lookup_host, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;

// ---------- config ----------

#[derive(Deserialize, Clone, Debug)]
pub struct ProbeConfig {
    pub name: String,
    /// "tcp" | "dns" | "pg" — discriminator.
    pub kind: String,
    /// For "tcp"/"tls": `host:port`. For "dns": hostname.
    #[serde(default)]
    pub target: Option<String>,
    /// For "pg"/"mysql": connection URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Interval between runs, in seconds (default 5).
    #[serde(default = "default_interval")]
    pub interval: u64,
    /// Per-attempt timeout, in seconds (default 5).
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_interval() -> u64 {
    5
}
fn default_timeout() -> u64 {
    5
}

impl ProbeConfig {
    pub fn target_display(&self) -> String {
        match (&self.target, &self.url) {
            (Some(t), _) => t.clone(),
            (None, Some(u)) => sanitize_url(u),
            (None, None) => "—".into(),
        }
    }
}

/// Mask the password in a URL for safe display in the UI: after
/// `://user:` everything up to `@` is replaced with `****`.
fn sanitize_url(u: &str) -> String {
    if let Some(idx) = u.find("://") {
        let (scheme, rest) = u.split_at(idx + 3);
        if let Some(at) = rest.find('@') {
            let userinfo = &rest[..at];
            let after = &rest[at..];
            if let Some(colon) = userinfo.find(':') {
                let user = &userinfo[..colon];
                return format!("{scheme}{user}:****{after}");
            }
        }
    }
    u.to_string()
}

// ---------- runtime state ----------

#[derive(Clone, Debug, Default)]
pub struct ProbeState {
    pub name: String,
    pub kind: String,
    pub target_display: String,
    pub last_ok: Option<Instant>,
    pub last_latency: Option<Duration>,
    pub last_error: Option<String>,
    pub consecutive_errors: u32,
    /// Rolling count of total runs and successful runs.
    pub total_runs: u64,
    pub total_ok: u64,
}

impl ProbeState {
    pub fn from_config(cfg: &ProbeConfig) -> Self {
        Self {
            name: cfg.name.clone(),
            kind: cfg.kind.clone(),
            target_display: cfg.target_display(),
            ..Default::default()
        }
    }
    pub fn success_pct(&self) -> Option<f32> {
        if self.total_runs == 0 {
            None
        } else {
            Some(100.0 * self.total_ok as f32 / self.total_runs as f32)
        }
    }
}

// ---------- channel message ----------

pub struct ProbeMsg {
    pub probe_idx: usize,
    pub result: Result<Duration, String>,
}

// ---------- spawn ----------

pub fn spawn_probes(
    probes: Vec<ProbeConfig>,
    tx: mpsc::Sender<ProbeMsg>,
) -> Vec<tokio::task::JoinHandle<()>> {
    probes
        .into_iter()
        .enumerate()
        .map(|(idx, cfg)| tokio::spawn(run_probe_loop(idx, cfg, tx.clone())))
        .collect()
}

async fn run_probe_loop(idx: usize, cfg: ProbeConfig, tx: mpsc::Sender<ProbeMsg>) {
    let interval = Duration::from_secs(cfg.interval.max(1));
    let deadline = Duration::from_secs(cfg.timeout.max(1));
    loop {
        let result = run_probe(&cfg, deadline)
            .await
            .map_err(|e| format!("{e:#}"));
        if tx
            .send(ProbeMsg {
                probe_idx: idx,
                result,
            })
            .await
            .is_err()
        {
            break; // receiver gone — exit
        }
        tokio::time::sleep(interval).await;
    }
}

// ---------- probe implementations ----------

async fn run_probe(cfg: &ProbeConfig, deadline: Duration) -> Result<Duration> {
    match cfg.kind.as_str() {
        "tcp" => run_tcp(cfg.target.as_deref(), deadline).await,
        "dns" => run_dns(cfg.target.as_deref(), deadline).await,
        "pg" => run_pg(cfg.url.as_deref(), deadline).await,
        other => Err(anyhow!("unknown probe kind '{}': expected tcp/dns/pg", other)),
    }
}

async fn run_tcp(target: Option<&str>, deadline: Duration) -> Result<Duration> {
    let target = target.ok_or_else(|| anyhow!("tcp probe: target=host:port required"))?;
    let start = Instant::now();
    timeout(deadline, TcpStream::connect(target))
        .await
        .with_context(|| format!("tcp connect timeout: {}", target))?
        .with_context(|| format!("tcp connect failed: {}", target))?;
    Ok(start.elapsed())
}

async fn run_dns(target: Option<&str>, deadline: Duration) -> Result<Duration> {
    let target = target.ok_or_else(|| anyhow!("dns probe: target=hostname required"))?;
    let q = if target.contains(':') {
        target.to_string()
    } else {
        // lookup_host requires host:port — append a dummy port 0.
        format!("{target}:0")
    };
    let start = Instant::now();
    let mut iter = timeout(deadline, lookup_host(q.clone()))
        .await
        .with_context(|| format!("dns resolve timeout: {}", target))?
        .with_context(|| format!("dns resolve failed: {}", target))?;
    if iter.next().is_none() {
        return Err(anyhow!("dns: zero results for {}", target));
    }
    Ok(start.elapsed())
}

async fn run_pg(url: Option<&str>, deadline: Duration) -> Result<Duration> {
    use tokio_postgres::NoTls;
    let url = url.ok_or_else(|| anyhow!("pg probe: url=postgres://... required"))?;
    let enriched = crate::dbcreds::enrich_pg_url(url);
    let start = Instant::now();
    let (client, connection) = timeout(deadline, tokio_postgres::connect(&enriched, NoTls))
        .await
        .context("pg probe: connect timeout")?
        .context("pg probe: connect failed")?;
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    let q = timeout(deadline, client.simple_query("SELECT 1"))
        .await
        .context("pg probe: SELECT 1 timeout")?;
    drop(client);
    task.abort();
    q.context("pg probe: SELECT 1 failed")?;
    Ok(start.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_url_password() {
        let u = "postgres://user:topsecret@host:5432/db";
        assert_eq!(sanitize_url(u), "postgres://user:****@host:5432/db");
    }
    #[test]
    fn sanitize_passthrough_when_no_password() {
        let u = "postgres://user@host/db";
        assert_eq!(sanitize_url(u), u);
    }
    #[test]
    fn target_display_falls_through_to_url() {
        let cfg = ProbeConfig {
            name: "x".into(),
            kind: "pg".into(),
            target: None,
            url: Some("postgres://u:p@h/db".into()),
            interval: 5,
            timeout: 5,
        };
        assert_eq!(cfg.target_display(), "postgres://u:****@h/db");
    }
    #[test]
    fn target_display_uses_target_field() {
        let cfg = ProbeConfig {
            name: "x".into(),
            kind: "tcp".into(),
            target: Some("db:5432".into()),
            url: None,
            interval: 5,
            timeout: 5,
        };
        assert_eq!(cfg.target_display(), "db:5432");
    }
}
