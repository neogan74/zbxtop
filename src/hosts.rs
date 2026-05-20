//! v0.5a — конфигурация мульти-хостового режима.
//!
//! Поддерживаются два способа задания списка хостов:
//! 1. **Config-файл** `~/.config/ztop/hosts.toml` (или путь из `--config`).
//! 2. **Single-host через CLI** (`--host` + остальные флаги) — для обратной
//!    совместимости и быстрых запусков на одну машину.
//!
//! Конфиг-файл выигрывает: если он есть и доступен — используется он, CLI
//! single-host флаги игнорируются (с предупреждением). Если конфига нет —
//! строим список из одного хоста по CLI.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Clone, Default)]
pub struct HostsConfig {
    #[serde(rename = "host", default)]
    pub hosts: Vec<HostConfig>,
    /// v0.7: глобальные пробы (TCP/DNS/PG). Опциональные — могут быть пустыми.
    #[serde(rename = "probe", default)]
    pub probes: Vec<crate::probes::ProbeConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct HostConfig {
    /// Имя для отображения в UI (может быть произвольным, не SSH-хостнейм).
    pub name: String,
    /// SSH-таргет (alias из ~/.ssh/config либо user@host).
    pub ssh: String,
    #[serde(default = "default_log_path")]
    pub log: String,
    #[serde(default)]
    pub sudo: bool,
    /// Опциональный override хоста для zabbix.stats (по умолчанию — отрезаем
    /// user@ из ssh-таргета).
    #[serde(default)]
    pub stats_host: Option<String>,
    #[serde(default = "default_stats_port")]
    pub stats_port: u16,
    #[serde(default)]
    pub no_stats: bool,
    /// DB connection URL. Если не задан — DB-источник для этого хоста выключен.
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
    /// Загрузить из произвольного пути.
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

    /// Дефолтный путь: `~/.config/ztop/hosts.toml`.
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
