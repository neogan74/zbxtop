//! v0.5a — multi-host mode configuration.
//!
//! Two ways to specify the host list are supported:
//! 1. **Config file** `~/.config/ztop/hosts.toml` (or path from `--config`).
//! 2. **Single-host via CLI** (`--host` + other flags) — for backward
//!    compatibility and quick single-machine runs.
//!
//! The config file takes priority: if it exists and is accessible, it is used
//! and CLI single-host flags are ignored (with a warning). If there is no
//! config file, the host list is built from CLI arguments.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Clone, Default)]
pub struct HostsConfig {
    #[serde(rename = "host", default)]
    pub hosts: Vec<HostConfig>,
    /// v0.7: global probes (TCP/DNS/PG). Optional — may be empty.
    #[serde(rename = "probe", default)]
    pub probes: Vec<crate::probes::ProbeConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct HostConfig {
    /// Display name in the UI (arbitrary, does not have to match the SSH hostname).
    pub name: String,
    /// SSH target (alias from ~/.ssh/config or user@host).
    pub ssh: String,
    #[serde(default = "default_log_path")]
    pub log: String,
    #[serde(default)]
    pub sudo: bool,
    /// Optional host override for zabbix.stats (defaults to stripping
    /// user@ from the ssh target).
    #[serde(default)]
    pub stats_host: Option<String>,
    #[serde(default = "default_stats_port")]
    pub stats_port: u16,
    #[serde(default)]
    pub no_stats: bool,
    /// DB connection URL. If not set, the DB source for this host is disabled.
    #[serde(default)]
    pub db_url: Option<String>,
    #[serde(default)]
    pub db_insecure_tls: bool,
}

fn default_log_path() -> String {
    "/var/log/zabbix/zabbix_server.log".to_string()
}

fn default_stats_port() -> u16 {
    10051
}

impl HostsConfig {
    /// Load from an arbitrary path.
    pub fn load_from(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("read config: {}", path.display()))?;
        let cfg: HostsConfig =
            toml::from_str(&content).with_context(|| format!("parse toml: {}", path.display()))?;
        if cfg.hosts.is_empty() {
            return Err(anyhow!(
                "{}: [[host]] section list is empty",
                path.display()
            ));
        }
        Ok(cfg)
    }

    /// Default path: `~/.config/ztop/hosts.toml`.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("ztop").join("hosts.toml"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_toml() {
        let s = r#"
[[host]]
name = "prod"
ssh = "zbx-prod-01"

[[host]]
name = "stage"
ssh = "ztop@zbx-stage"
log = "/srv/zabbix/log/server.log"
sudo = true
no_stats = true
db_url = "postgres://ztop_ro:secret@db/zbx?sslmode=require"
"#;
        let cfg: HostsConfig = toml::from_str(s).unwrap();
        assert_eq!(cfg.hosts.len(), 2);
        assert_eq!(cfg.hosts[0].name, "prod");
        assert_eq!(cfg.hosts[0].log, "/var/log/zabbix/zabbix_server.log");
        assert_eq!(cfg.hosts[0].stats_port, 10051);
        assert!(!cfg.hosts[0].sudo);
        assert!(cfg.hosts[1].sudo);
        assert!(cfg.hosts[1].no_stats);
        assert!(cfg.hosts[1].db_url.is_some());
    }

    #[test]
    fn empty_hosts_is_error() {
        let path = std::env::temp_dir().join("ztop_test_empty.toml");
        std::fs::write(&path, "").unwrap();
        let err = HostsConfig::load_from(&path).err().unwrap();
        assert!(err.to_string().contains("empty"));
        let _ = std::fs::remove_file(&path);
    }
}
