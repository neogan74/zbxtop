//! v0.4b — алерт-аккорд: правила, которые опираются на ДВА и более источника.
//!
//! Каждое правило само по себе ничего нового не показывает — но пересечение
//! сигналов из разных слоёв (OS / zabbix.stats / DB) даёт диагноз, который
//! нельзя получить, глядя только на один таб.
//!
//! Пример: `history syncer busy = 95%` (stats) **И** «много insert-ов в
//! `history*` в waiting» (DB) → значит история не успевает уходить в БД.
//! Только зная оба факта, можем поставить диагноз.

use crate::app::HostState;
use crate::collectors::ZbxRoleAgg;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

#[derive(Clone, Debug)]
pub struct Diagnosis {
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    /// Какие источники участвовали в принятии решения. Полезно для отладки и
    /// для пометки «диагноз стал недостоверен, потому что один источник упал».
    pub sources: Vec<&'static str>,
}

/// Запустить все правила. Возвращает только сработавшие диагнозы,
/// отсортированные по убыванию severity. Дублей нет.
pub fn diagnose(app: &HostState) -> Vec<Diagnosis> {
    let mut out: Vec<Diagnosis> = [
        rule_history_backed_up(app),
        rule_db_lock_contention(app),
        rule_idle_in_tx_zombie(app),
        rule_queue_vs_idle(app),
        rule_busy_cpu_delta(app),
        rule_replication_lag(app),
    ]
    .into_iter()
    .flatten()
    .collect();
    // Severity desc, затем по title для стабильности
    out.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.title.cmp(&b.title)));
    out
}

// ---------- rules ----------

/// «История не успевает в БД»: history syncer от stats > 85% busy,
/// и в DB есть waiting/lock-event-ы на history-таблицах или connections.waiting > 0.
fn rule_history_backed_up(app: &HostState) -> Option<Diagnosis> {
    let stats = app.stats.as_ref()?;
    let hs = stats.process.get("history syncer")?;
    if hs.busy.avg < 85.0 {
        return None;
    }

    let db = app.db.as_ref()?;
    let waiting_inserts = db
        .top_queries
        .iter()
        .filter(|q| {
            q.query.to_lowercase().contains("history")
                && (q.wait_event.is_some() || q.state.contains("active"))
        })
        .count();
    let conn_waiting = db.connections.waiting;

    if waiting_inserts == 0 && conn_waiting == 0 {
        return None;
    }
    Some(Diagnosis {
        severity: Severity::Critical,
        title: "history not landing in DB".into(),
        detail: format!(
            "history syncer busy {:.0}% + {} queries on history* waiting (DB conn waiting={})",
            hs.busy.avg, waiting_inserts, conn_waiting
        ),
        sources: vec!["stats", "db"],
    })
}

/// «Lock contention в БД»: locks_waiting > 0 и/или top-query c wait_event="Lock:..."
fn rule_db_lock_contention(app: &HostState) -> Option<Diagnosis> {
    let db = app.db.as_ref()?;
    let with_lock_event = db
        .top_queries
        .iter()
        .filter(|q| {
            q.wait_event
                .as_deref()
                .map(|e| e.starts_with("Lock"))
                .unwrap_or(false)
        })
        .count();
    if db.locks_waiting == 0 && with_lock_event == 0 {
        return None;
    }
    let sev = if db.locks_waiting >= 5 || with_lock_event >= 3 {
        Severity::Critical
    } else {
        Severity::Warning
    };
    Some(Diagnosis {
        severity: sev,
        title: "DB lock contention".into(),
        detail: format!(
            "{} backends waiting on locks, {} queries with Lock:* wait_event",
            db.locks_waiting, with_lock_event
        ),
        sources: vec!["db"],
    })
}

/// «Idle-in-transaction зомби»: idle-in-tx коннект, висящий > 60 сек.
/// Это типичная причина, по которой housekeeper или autovacuum не может
/// почистить старые версии строк.
fn rule_idle_in_tx_zombie(app: &HostState) -> Option<Diagnosis> {
    let db = app.db.as_ref()?;
    let zombies: Vec<_> = db
        .top_queries
        .iter()
        .filter(|q| q.state.starts_with("idle in transaction") && q.age_sec >= 60.0)
        .collect();
    if zombies.is_empty() {
        return None;
    }
    let max_age = zombies
        .iter()
        .map(|q| q.age_sec)
        .fold(0.0_f64, f64::max);
    let sev = if max_age >= 600.0 {
        Severity::Critical
    } else {
        Severity::Warning
    };
    Some(Diagnosis {
        severity: sev,
        title: "idle-in-transaction zombie".into(),
        detail: format!(
            "{} backends stuck in idle-in-tx (oldest {:.0}s) — housekeeper/autovacuum will be blocked",
            zombies.len(),
            max_age
        ),
        sources: vec!["db"],
    })
}

/// «Очередь растёт, а процессы простаивают»: stats.queue > 1000,
/// при этом никакой процесс не busy > 30%.
fn rule_queue_vs_idle(app: &HostState) -> Option<Diagnosis> {
    let stats = app.stats.as_ref()?;
    let queue = stats.queue_total()?;
    if queue < 1000 {
        return None;
    }
    let max_busy = stats
        .process
        .values()
        .map(|p| p.busy.avg)
        .fold(0.0_f32, f32::max);
    if max_busy >= 30.0 {
        return None;
    }
    Some(Diagnosis {
        severity: if queue >= 10_000 {
            Severity::Critical
        } else {
            Severity::Warning
        },
        title: "queue grows while workers idle".into(),
        detail: format!(
            "queue={}, top process busy {:.0}% — capacity mismatch, item config error, or external dependency"
            , queue, max_busy
        ),
        sources: vec!["stats"],
    })
}

/// «Процесс ждёт, не считает»: для какой-то роли busy от stats сильно
/// больше, чем «busy» от ps (доля не-idle форков по proctitle).
fn rule_busy_cpu_delta(app: &HostState) -> Option<Diagnosis> {
    let stats = app.stats.as_ref()?;
    let mut worst: Option<(String, f32)> = None;
    for r in &app.roles {
        let busy_ps = if r.count == 0 {
            0.0
        } else {
            100.0 * r.busy as f32 / r.count as f32
        };
        if let Some(p) = stats.process.get(&r.role) {
            let delta = p.busy.avg - busy_ps;
            if delta >= 30.0 {
                if worst
                    .as_ref()
                    .map(|w| delta > w.1)
                    .unwrap_or(true)
                {
                    worst = Some((r.role.clone(), delta));
                }
            }
        }
    }
    let (role, delta) = worst?;
    Some(Diagnosis {
        severity: if delta >= 50.0 {
            Severity::Critical
        } else {
            Severity::Warning
        },
        title: format!("{role} waiting, not running"),
        detail: format!(
            "Δ busy(zbx) - busy(ps) = +{:.0}% — forks are stuck on lock/DB/IO, not CPU-bound",
            delta
        ),
        sources: vec!["stats", "ps"],
    })
}

/// Replication lag > 30 секунд.
fn rule_replication_lag(app: &HostState) -> Option<Diagnosis> {
    let db = app.db.as_ref()?;
    let lag = db.replication_lag_sec?;
    if lag < 30.0 {
        return None;
    }
    Some(Diagnosis {
        severity: if lag >= 300.0 {
            Severity::Critical
        } else {
            Severity::Warning
        },
        title: "replica lag".into(),
        detail: format!("{:.0}s behind primary", lag),
        sources: vec!["db"],
    })
}

// Заглушка: чтобы линтер видел импорт.
#[allow(dead_code)]
fn _link_helper(_: &ZbxRoleAgg) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::ZbxRoleAgg;
    use crate::db::{ConnectionStats, DbStats, LongQuery};
    use crate::zbxstats::{BusyStats, ProcessStats, ZabbixStats};

    fn host_with(
        stats: Option<ZabbixStats>,
        db: Option<DbStats>,
        roles: Vec<ZbxRoleAgg>,
    ) -> HostState {
        let mut h = HostState::new(
            "h".into(),
            "h".into(),
            "/log".into(),
            false,
            true,
            true,
        );
        h.stats = stats;
        h.db = db;
        h.roles = roles;
        h
    }

    fn proc(avg: f32, count: u32) -> ProcessStats {
        ProcessStats {
            busy: BusyStats {
                avg,
                max: avg,
                min: avg,
            },
            count,
        }
    }

    #[test]
    fn detects_history_backed_up() {
        let mut s = ZabbixStats::default();
        s.process.insert("history syncer".into(), proc(95.0, 4));
        let mut d = DbStats::default();
        d.top_queries.push(LongQuery {
            pid: 1,
            usename: "zabbix".into(),
            state: "active".into(),
            wait_event: Some("IO:DataFileRead".into()),
            age_sec: 5.0,
            query: "INSERT INTO history VALUES (...)".into(),
        });
        let host =host_with(Some(s), Some(d), vec![]);
        let diag = diagnose(&host);
        assert!(diag.iter().any(|x| x.title.contains("history not landing")));
    }

    #[test]
    fn detects_idle_in_tx_zombie() {
        let mut d = DbStats::default();
        d.top_queries.push(LongQuery {
            pid: 9,
            usename: "zbx".into(),
            state: "idle in transaction".into(),
            wait_event: None,
            age_sec: 700.0,
            query: "...".into(),
        });
        let host =host_with(None, Some(d), vec![]);
        let diag = diagnose(&host);
        let z = diag.iter().find(|x| x.title.contains("idle-in-transaction")).unwrap();
        assert_eq!(z.severity, Severity::Critical);
    }

    #[test]
    fn busy_cpu_delta_when_role_blocks() {
        let mut s = ZabbixStats::default();
        s.process.insert("poller".into(), proc(90.0, 8));
        let role = ZbxRoleAgg {
            role: "poller".into(),
            count: 8,
            busy: 1, // 12.5% по ps
            cpu_sum: 5.0,
            rss_sum_kb: 0,
            sample_status: String::new(),
        };
        let host =host_with(Some(s), None, vec![role]);
        let diag = diagnose(&host);
        assert!(diag.iter().any(|x| x.title.contains("poller waiting")));
    }

    #[test]
    fn no_alerts_when_healthy() {
        let mut s = ZabbixStats::default();
        s.process.insert("poller".into(), proc(5.0, 8));
        s.queue = serde_json::json!(10);
        let mut d = DbStats::default();
        d.connections.idle = 5;
        d.replication_lag_sec = Some(0.1);
        let host =host_with(Some(s), Some(d), vec![]);
        assert!(diagnose(&host).is_empty());
    }
}
