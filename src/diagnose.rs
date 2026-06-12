//! v0.4b — alert chord: rules that rely on TWO or more sources.
//!
//! Each rule by itself shows nothing new — but the intersection of signals
//! from different layers (OS / zabbix.stats / DB) produces a diagnosis that
//! cannot be obtained by looking at a single tab alone.
//!
//! Example: `history syncer busy = 95%` (stats) **AND** "many inserts into
//! `history*` in waiting" (DB) → the history writer cannot keep up with the DB.
//! Only knowing both facts can we make the diagnosis.

use crate::app::HostState;
use crate::zbxstats::CacheName;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    #[allow(dead_code)]
    Info,
    Warning,
    Critical,
}

#[derive(Clone, Debug)]
pub struct Diagnosis {
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    /// Which sources contributed to the decision. Useful for debugging and
    /// for flagging "the diagnosis became unreliable because one source went down".
    pub sources: Vec<&'static str>,
}

/// Run all rules. Returns only the triggered diagnoses,
/// sorted by descending severity. No duplicates.
pub fn diagnose(app: &HostState) -> Vec<Diagnosis> {
    let mut out: Vec<Diagnosis> = [
        rule_history_backed_up(app),
        rule_db_lock_contention(app),
        rule_idle_in_tx_zombie(app),
        rule_queue_vs_idle(app),
        rule_busy_cpu_delta(app),
        rule_replication_lag(app),
        rule_cache_full(app),
        rule_housekeeper_blocked(app),
        rule_trapper_saturation(app),
        rule_alerter_bottleneck(app),
    ]
    .into_iter()
    .flatten()
    .collect();
    // Severity desc, then by title for stability
    out.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.title.cmp(&b.title)));
    out
}

// ---------- rules ----------

/// "History can't keep up with DB": history syncer from stats > 85% busy,
/// and DB has waiting/lock events on history tables or connections.waiting > 0.
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

/// "DB lock contention": locks_waiting > 0 and/or a top query with wait_event="Lock:..."
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

/// "Idle-in-transaction zombie": an idle-in-tx connection stuck for > 60 sec.
/// This is the typical reason housekeeper or autovacuum cannot clean up
/// old row versions.
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

/// "Queue growing while workers idle": stats.queue > 1000,
/// yet no process is busy > 30%.
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

/// "Process waiting, not computing": for some role the busy% from stats is
/// significantly higher than "busy" from ps (fraction of non-idle forks by proctitle).
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
            if delta >= 30.0
                && worst
                    .as_ref()
                    .map(|w| delta > w.1)
                    .unwrap_or(true)
            {
                worst = Some((r.role.clone(), delta));
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

/// "Write/value cache full": wcache or vcache used > 95%.
/// Items arriving faster than they can be written to DB — old data may be overwritten.
fn rule_cache_full(app: &HostState) -> Option<Diagnosis> {
    let stats = app.stats.as_ref()?;
    let wcache = stats.cache_used_pct(CacheName::Write);
    let vcache = stats.cache_used_pct(CacheName::Value);

    let worst = [("write cache", wcache), ("value cache", vcache)]
        .into_iter()
        .filter_map(|(name, pct)| pct.map(|p| (name, p)))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))?;

    if worst.1 < 95.0 {
        return None;
    }
    Some(Diagnosis {
        severity: if worst.1 >= 99.0 {
            Severity::Critical
        } else {
            Severity::Warning
        },
        title: format!("{} full", worst.0),
        detail: format!(
            "{} used {:.0}% — history values may be dropped or overwritten before landing in DB",
            worst.0, worst.1
        ),
        sources: vec!["stats"],
    })
}

/// "Housekeeper blocked": housekeeper busy > 80% while idle-in-tx connections
/// or lock contention prevent row cleanup.
fn rule_housekeeper_blocked(app: &HostState) -> Option<Diagnosis> {
    let stats = app.stats.as_ref()?;
    let hk = stats.process.get("housekeeper")?;
    if hk.busy.avg < 80.0 {
        return None;
    }

    let db = app.db.as_ref()?;
    let zombies = db.connections.idle_in_transaction;
    let locks = db.locks_waiting;
    if zombies == 0 && locks == 0 {
        return None;
    }
    Some(Diagnosis {
        severity: if hk.busy.avg >= 95.0 {
            Severity::Critical
        } else {
            Severity::Warning
        },
        title: "housekeeper blocked".into(),
        detail: format!(
            "housekeeper busy {:.0}% but {} idle-in-tx + {} lock waiters prevent row cleanup",
            hk.busy.avg, zombies, locks
        ),
        sources: vec!["stats", "db"],
    })
}

/// "Trapper saturated": trapper busy > 85% and queue already building up.
/// Incoming data rate exceeds the server's acceptance capacity.
fn rule_trapper_saturation(app: &HostState) -> Option<Diagnosis> {
    let stats = app.stats.as_ref()?;
    let trapper = stats.process.get("trapper")?;
    if trapper.busy.avg < 85.0 {
        return None;
    }
    let queue = stats.queue_total().unwrap_or(0);
    if queue < 500 {
        return None;
    }
    Some(Diagnosis {
        severity: if trapper.busy.avg >= 95.0 || queue >= 5_000 {
            Severity::Critical
        } else {
            Severity::Warning
        },
        title: "trapper saturated".into(),
        detail: format!(
            "trapper busy {:.0}% with queue={} — incoming metric rate exceeds acceptance capacity",
            trapper.busy.avg, queue
        ),
        sources: vec!["stats"],
    })
}

/// "Alerter bottleneck": alerter busy > 85% while DB has queries with wait events.
/// Notification delivery is stalled because DB inserts/selects can't complete.
fn rule_alerter_bottleneck(app: &HostState) -> Option<Diagnosis> {
    let stats = app.stats.as_ref()?;
    let alerter = stats.process.get("alerter")?;
    if alerter.busy.avg < 85.0 {
        return None;
    }

    let db = app.db.as_ref()?;
    let db_waiters = db
        .top_queries
        .iter()
        .filter(|q| q.wait_event.is_some())
        .count();
    if db_waiters == 0 && db.connections.waiting == 0 {
        return None;
    }
    Some(Diagnosis {
        severity: if alerter.busy.avg >= 95.0 {
            Severity::Critical
        } else {
            Severity::Warning
        },
        title: "alerter bottleneck".into(),
        detail: format!(
            "alerter busy {:.0}% + {} DB queries waiting — alert notifications stalled",
            alerter.busy.avg, db_waiters
        ),
        sources: vec!["stats", "db"],
    })
}

/// Replication lag > 30 seconds.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::ZbxRoleAgg;
    use crate::db::{DbStats, LongQuery};
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
    fn detects_cache_full_warning() {
        let mut s = ZabbixStats::default();
        // Inject wcache with pfree = 3  → used = 97%
        s.wcache = serde_json::json!({"pfree": 3.0});
        let host = host_with(Some(s), None, vec![]);
        let diag = diagnose(&host);
        let d = diag.iter().find(|x| x.title.contains("write cache full")).unwrap();
        assert_eq!(d.severity, Severity::Warning);
    }

    #[test]
    fn detects_cache_full_critical() {
        let mut s = ZabbixStats::default();
        s.vcache = serde_json::json!({"buffer": {"pfree": 0.5}});
        let host = host_with(Some(s), None, vec![]);
        let diag = diagnose(&host);
        let d = diag.iter().find(|x| x.title.contains("value cache full")).unwrap();
        assert_eq!(d.severity, Severity::Critical);
    }

    #[test]
    fn detects_housekeeper_blocked() {
        let mut s = ZabbixStats::default();
        s.process.insert("housekeeper".into(), proc(90.0, 1));
        let mut d = DbStats::default();
        d.connections.idle_in_transaction = 2;
        let host = host_with(Some(s), Some(d), vec![]);
        let diag = diagnose(&host);
        assert!(diag.iter().any(|x| x.title == "housekeeper blocked"));
    }

    #[test]
    fn housekeeper_ok_without_blockers() {
        let mut s = ZabbixStats::default();
        s.process.insert("housekeeper".into(), proc(90.0, 1));
        let mut d = DbStats::default();
        d.connections.idle_in_transaction = 0;
        d.locks_waiting = 0;
        let host = host_with(Some(s), Some(d), vec![]);
        assert!(!diagnose(&host).iter().any(|x| x.title == "housekeeper blocked"));
    }

    #[test]
    fn detects_trapper_saturation() {
        let mut s = ZabbixStats::default();
        s.process.insert("trapper".into(), proc(92.0, 5));
        s.queue = serde_json::json!(3000u64);
        let host = host_with(Some(s), None, vec![]);
        let diag = diagnose(&host);
        assert!(diag.iter().any(|x| x.title == "trapper saturated"));
    }

    #[test]
    fn trapper_busy_but_queue_small_no_alert() {
        let mut s = ZabbixStats::default();
        s.process.insert("trapper".into(), proc(90.0, 5));
        s.queue = serde_json::json!(100u64);
        let host = host_with(Some(s), None, vec![]);
        assert!(!diagnose(&host).iter().any(|x| x.title == "trapper saturated"));
    }

    #[test]
    fn detects_alerter_bottleneck() {
        let mut s = ZabbixStats::default();
        s.process.insert("alerter".into(), proc(88.0, 1));
        let mut d = DbStats::default();
        d.top_queries.push(LongQuery {
            pid: 42,
            usename: "zabbix".into(),
            state: "active".into(),
            wait_event: Some("Lock:relation".into()),
            age_sec: 3.0,
            query: "INSERT INTO alerts ...".into(),
        });
        let host = host_with(Some(s), Some(d), vec![]);
        let diag = diagnose(&host);
        assert!(diag.iter().any(|x| x.title == "alerter bottleneck"));
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
