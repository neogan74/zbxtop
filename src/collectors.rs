//! Парсеры удалённых данных: процессы zabbix_server, /proc, логи.
//!
//! Все коллекторы — чистые функции от строки stdout к структурам.
//! Это позволяет тестировать их офлайн (есть unit-тесты с зашитыми примерами).

use crate::ssh::{run, SshTarget};
use anyhow::Result;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------- Процессы zabbix_server ----------

/// Одна строка `ps` для процесса zabbix_server.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZbxProc {
    pub pid: u32,
    pub cpu: f32, // %
    pub mem: f32, // %
    pub rss_kb: u64,
    pub etimes: u64,
    /// Чистая роль, например "poller", "history syncer", "trapper".
    pub role: String,
    /// Текст в квадратных скобках из proctitle — статус: "got 1 values...", "idle 3 sec".
    pub status: String,
    /// True, если строка статуса говорит об idle (нет работы прямо сейчас).
    pub idle: bool,
}

/// Агрегат по роли.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ZbxRoleAgg {
    pub role: String,
    pub count: u32,
    pub cpu_sum: f32,
    pub rss_sum_kb: u64,
    pub busy: u32,
    pub sample_status: String,
}

/// Регекс выделяет: pid, %cpu, %mem, rss(KB), etimes (секунды от старта), args.
static PS_LINE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\s*(?P<pid>\d+)\s+(?P<cpu>[\d.]+)\s+(?P<mem>[\d.]+)\s+(?P<rss>\d+)\s+(?P<etimes>\d+)\s+(?P<args>.+)$").unwrap()
});

/// Из args выделяем "zabbix_server: <role> #N [status...]" или
/// "zabbix_proxy: <role> #N [status...]". Роль может быть многословной
/// ("history syncer", "data sender", "vmware collector", "lld manager",
/// "preprocessing worker").
static TITLE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^zabbix_(?:server|proxy):\s+(?P<role>[a-zA-Z][a-zA-Z _-]+?)(?:\s+#\d+)?(?:\s+\[(?P<status>[^\]]*)\])?\s*$").unwrap()
});

pub fn parse_ps(output: &str) -> Vec<ZbxProc> {
    let mut out = Vec::new();
    for line in output.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let Some(caps) = PS_LINE.captures(line) else {
            continue;
        };
        let args = caps.name("args").map(|m| m.as_str()).unwrap_or("");
        let Some(title) = TITLE.captures(args) else {
            // Это, скорее всего, родительский /usr/sbin/zabbix_server, его пропускаем —
            // он отдельной строки в дашборде не заслуживает.
            continue;
        };
        let role = title["role"].trim().to_string();
        let status = title
            .name("status")
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        let idle = status.contains("idle") || status.contains("waiting for");
        out.push(ZbxProc {
            pid: caps["pid"].parse().unwrap_or(0),
            cpu: caps["cpu"].parse().unwrap_or(0.0),
            mem: caps["mem"].parse().unwrap_or(0.0),
            rss_kb: caps["rss"].parse().unwrap_or(0),
            etimes: caps["etimes"].parse().unwrap_or(0),
            role,
            status,
            idle,
        });
    }
    out
}

pub fn aggregate_roles(procs: &[ZbxProc]) -> Vec<ZbxRoleAgg> {
    let mut map: BTreeMap<String, ZbxRoleAgg> = BTreeMap::new();
    for p in procs {
        let agg = map.entry(p.role.clone()).or_insert_with(|| ZbxRoleAgg {
            role: p.role.clone(),
            ..Default::default()
        });
        agg.count += 1;
        agg.cpu_sum += p.cpu;
        agg.rss_sum_kb += p.rss_kb;
        if !p.idle {
            agg.busy += 1;
        }
        if agg.sample_status.is_empty() {
            agg.sample_status = p.status.clone();
        }
    }
    let mut v: Vec<_> = map.into_values().collect();
    // Сортируем по убыванию суммарного CPU — самые активные сверху.
    v.sort_by(|a, b| b.cpu_sum.partial_cmp(&a.cpu_sum).unwrap_or(std::cmp::Ordering::Equal));
    v
}

pub async fn fetch_procs(t: &SshTarget) -> Result<Vec<ZbxProc>> {
    // `ww` — не обрезать длинные args. Заголовок отключаем через `--no-headers` (procps);
    // на BSD-ps его нет, поэтому используем awk-фильтр зачистки.
    let out = run(
        t,
        "ps -eo pid,pcpu,pmem,rss,etimes,args ww 2>/dev/null | awk 'NR>1 && /zabbix_(server|proxy):/'",
    )
    .await?;
    Ok(parse_ps(&out))
}

// ---------- Системные метрики ----------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SysStats {
    pub load1: f32,
    pub load5: f32,
    pub load15: f32,
    pub mem_total_kb: u64,
    pub mem_available_kb: u64,
    pub swap_total_kb: u64,
    pub swap_free_kb: u64,
    pub uptime_sec: u64,
}

impl SysStats {
    pub fn mem_used_pct(&self) -> f32 {
        if self.mem_total_kb == 0 {
            0.0
        } else {
            100.0 * (1.0 - self.mem_available_kb as f32 / self.mem_total_kb as f32)
        }
    }
    pub fn swap_used_pct(&self) -> f32 {
        if self.swap_total_kb == 0 {
            0.0
        } else {
            100.0 * (1.0 - self.swap_free_kb as f32 / self.swap_total_kb as f32)
        }
    }
}

pub fn parse_loadavg(s: &str) -> (f32, f32, f32) {
    let parts: Vec<&str> = s.split_whitespace().collect();
    let p = |i: usize| parts.get(i).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    (p(0), p(1), p(2))
}

pub fn parse_meminfo(s: &str) -> (u64, u64, u64, u64) {
    // вернёт (mem_total, mem_available, swap_total, swap_free) в KiB
    let mut mt = 0u64;
    let mut ma = 0u64;
    let mut st = 0u64;
    let mut sf = 0u64;
    for line in s.lines() {
        let mut parts = line.split_whitespace();
        let key = parts.next().unwrap_or("");
        let val: u64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        match key {
            "MemTotal:" => mt = val,
            "MemAvailable:" => ma = val,
            "SwapTotal:" => st = val,
            "SwapFree:" => sf = val,
            _ => {}
        }
    }
    (mt, ma, st, sf)
}

pub fn parse_uptime(s: &str) -> u64 {
    s.split_whitespace()
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .map(|x| x as u64)
        .unwrap_or(0)
}

pub async fn fetch_sys(t: &SshTarget) -> Result<SysStats> {
    // Один SSH-вызов, три файла — экономим round-trips.
    let out = run(
        t,
        "echo '--LOADAVG--'; cat /proc/loadavg; echo '--MEMINFO--'; cat /proc/meminfo; echo '--UPTIME--'; cat /proc/uptime",
    )
    .await?;
    let mut stats = SysStats::default();
    let mut section = "";
    let mut load_buf = String::new();
    let mut mem_buf = String::new();
    let mut up_buf = String::new();
    for line in out.lines() {
        match line.trim() {
            "--LOADAVG--" => section = "load",
            "--MEMINFO--" => section = "mem",
            "--UPTIME--" => section = "up",
            _ => match section {
                "load" => load_buf.push_str(&format!("{line}\n")),
                "mem" => mem_buf.push_str(&format!("{line}\n")),
                "up" => up_buf.push_str(&format!("{line}\n")),
                _ => {}
            },
        }
    }
    let (l1, l5, l15) = parse_loadavg(&load_buf);
    stats.load1 = l1;
    stats.load5 = l5;
    stats.load15 = l15;
    let (mt, ma, st, sf) = parse_meminfo(&mem_buf);
    stats.mem_total_kb = mt;
    stats.mem_available_kb = ma;
    stats.swap_total_kb = st;
    stats.swap_free_kb = sf;
    stats.uptime_sec = parse_uptime(&up_buf);
    Ok(stats)
}

// ---------- Логи ----------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogLine {
    pub raw: String,
    pub level: LogLevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    Error,
    Warning,
    Info,
    Debug,
    Other,
}

pub fn classify(line: &str) -> LogLevel {
    // Zabbix не помечает уровень в каждой строке явно, но эвристики работают:
    let l = line.to_lowercase();
    if l.contains("[z3001]")
        || l.contains("cannot ")
        || l.contains("error")
        || l.contains("failed")
    {
        LogLevel::Error
    } else if l.contains("slow query")
        || l.contains("warning")
        || l.contains("housekeeper") && l.contains("delete")
    {
        LogLevel::Warning
    } else if l.contains("debug") {
        LogLevel::Debug
    } else if !line.is_empty() {
        LogLevel::Info
    } else {
        LogLevel::Other
    }
}

pub async fn fetch_log_tail(t: &SshTarget, path: &str, lines: usize) -> Result<Vec<LogLine>> {
    // shell-escape пути — на MVP полагаемся, что путь не содержит пробелов/кавычек.
    let cmd = format!("tail -n {lines} -- {path} 2>/dev/null");
    let out = run(t, &cmd).await?;
    Ok(out
        .lines()
        .map(|l| LogLine {
            level: classify(l),
            raw: l.to_string(),
        })
        .collect())
}

// ---------- Runtime control ----------

#[derive(Clone, Copy, Debug)]
pub enum RuntimeCmd {
    LogLevelIncrease,
    LogLevelDecrease,
    HousekeeperExecute,
    ConfigCacheReload,
    SnmpCacheReload,
    Diaginfo,
}

impl RuntimeCmd {
    pub fn arg(self) -> &'static str {
        match self {
            RuntimeCmd::LogLevelIncrease => "log_level_increase",
            RuntimeCmd::LogLevelDecrease => "log_level_decrease",
            RuntimeCmd::HousekeeperExecute => "housekeeper_execute",
            RuntimeCmd::ConfigCacheReload => "config_cache_reload",
            RuntimeCmd::SnmpCacheReload => "snmp_cache_reload",
            RuntimeCmd::Diaginfo => "diaginfo",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            RuntimeCmd::LogLevelIncrease => "Log level +1",
            RuntimeCmd::LogLevelDecrease => "Log level -1",
            RuntimeCmd::HousekeeperExecute => "Housekeeper run",
            RuntimeCmd::ConfigCacheReload => "Config reload",
            RuntimeCmd::SnmpCacheReload => "SNMP reload",
            RuntimeCmd::Diaginfo => "Diag info → log",
        }
    }
}

pub async fn runtime_control(t: &SshTarget, sudo: bool, cmd: RuntimeCmd) -> Result<String> {
    // sudo делается БЕЗ -S — пароля не будет, ssh BatchMode=yes; пользователь должен
    // настроить NOPASSWD для конкретной команды (см. README).
    let prefix = if sudo { "sudo -n " } else { "" };
    let full = format!("{prefix}zabbix_server -R {} 2>&1", cmd.arg());
    run(t, &full).await
}

// ---------- Тесты ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ps_lines() {
        let sample = "\
  1234  3.4  1.2 245678 12345 zabbix_server: poller #3 [got 1 values in 0.000034 sec, idle 1 sec]
  1240  0.1  0.5  98765 12340 zabbix_server: trapper #1 [waiting for connection]
  1250  9.8  2.1 412345 12000 zabbix_server: history syncer #2 [synced 42 items in 0.001234 sec, idle 1 sec]
  9999  0.0  0.0   1234 12345 /usr/sbin/zabbix_server -c /etc/zabbix/zabbix_server.conf
";
        let procs = parse_ps(sample);
        assert_eq!(procs.len(), 3);
        assert_eq!(procs[0].role, "poller");
        assert!(procs[0].idle);
        assert_eq!(procs[1].role, "trapper");
        assert!(procs[1].idle, "waiting for connection counts as idle");
        assert_eq!(procs[2].role, "history syncer");
    }

    #[test]
    fn parses_zabbix_proxy_lines() {
        let sample = "\
  2001  2.1  0.5 198765 54321 zabbix_proxy: poller #1 [got 5 values in 0.001 sec, idle 1 sec]
  2002  0.5  0.3  98765 54320 zabbix_proxy: data sender [sent 100 values, idle 5 sec]
  2003  0.1  0.2  87654 54310 zabbix_proxy: heartbeat sender [sending heartbeat]
  2004  1.0  0.6 124345 50000 zabbix_proxy: history syncer #1 [synced 12 items in 0.0005 sec, idle 1 sec]
";
        let procs = parse_ps(sample);
        assert_eq!(procs.len(), 4);
        assert_eq!(procs[0].role, "poller");
        assert_eq!(procs[1].role, "data sender");
        assert_eq!(procs[2].role, "heartbeat sender");
        assert_eq!(procs[3].role, "history syncer");
    }

    #[test]
    fn aggregates_by_role() {
        let procs = vec![
            ZbxProc {
                pid: 1,
                cpu: 2.0,
                mem: 0.5,
                rss_kb: 1000,
                etimes: 100,
                role: "poller".into(),
                status: "got 1 values in 0.001 sec, idle 1 sec".into(),
                idle: true,
            },
            ZbxProc {
                pid: 2,
                cpu: 3.0,
                mem: 0.5,
                rss_kb: 1500,
                etimes: 100,
                role: "poller".into(),
                status: "got 5 values in 0.01 sec".into(),
                idle: false,
            },
        ];
        let agg = aggregate_roles(&procs);
        assert_eq!(agg.len(), 1);
        assert_eq!(agg[0].count, 2);
        assert!((agg[0].cpu_sum - 5.0).abs() < 1e-3);
        assert_eq!(agg[0].busy, 1);
    }

    #[test]
    fn parses_meminfo() {
        let s = "MemTotal:       16384000 kB\nMemFree:         2000000 kB\nMemAvailable:    8000000 kB\nSwapTotal:        500000 kB\nSwapFree:         400000 kB\n";
        let (mt, ma, st, sf) = parse_meminfo(s);
        assert_eq!(mt, 16384000);
        assert_eq!(ma, 8000000);
        assert_eq!(st, 500000);
        assert_eq!(sf, 400000);
    }

    #[test]
    fn parses_loadavg() {
        let (a, b, c) = parse_loadavg("0.42 0.55 0.61 1/234 9876");
        assert!((a - 0.42).abs() < 1e-3);
        assert!((b - 0.55).abs() < 1e-3);
        assert!((c - 0.61).abs() < 1e-3);
    }

    #[test]
    fn classifies_log_levels() {
        assert_eq!(classify("12:00:00 [Z3001] connection failed"), LogLevel::Error);
        assert_eq!(classify("12:00:00 cannot connect to db"), LogLevel::Error);
        assert_eq!(classify("slow query took 5 seconds"), LogLevel::Warning);
        assert_eq!(classify("housekeeper deleted 1234 rows"), LogLevel::Warning);
        assert_eq!(classify("server started"), LogLevel::Info);
    }
}
