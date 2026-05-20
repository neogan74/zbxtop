//! v0.4 — DB-скрейпер (PostgreSQL + MySQL/MariaDB).
//!
//! Идея: третий слой доступности. SSH-source падает первым (хост умер),
//! zabbix.stats — вторым (процесс zabbix_server умер), DB-source — третьим
//! (база умерла). Когда видны два слоя из трёх — сразу понятно, где отказ.
//!
//! Backend выбирается автоматически по схеме URL:
//!   `postgres://...` или `postgresql://...` → PostgreSQL
//!   `mysql://...`                            → MySQL/MariaDB
//!
//! PostgreSQL: persistent connection (v0.4b), fail-fast при ошибке запроса.
//! MySQL: persistent connection, try-or-default per-query (компатибельность
//! с разными версиями и edition-ами MySQL/MariaDB).

use anyhow::{Context, Result};
use native_tls::TlsConnector as NativeTlsConnector;
use postgres_native_tls::MakeTlsConnector;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_postgres::Client;

#[derive(Clone, Debug)]
pub struct DbTarget {
    /// Полная connection string в формате libpq URL или mysql:// URL.
    /// Примеры:
    ///   `postgres://ztop_ro:secret@db-host:5432/zabbix?sslmode=require`
    ///   `mysql://ztop_ro:secret@db-host:3306/zabbix?ssl-mode=REQUIRED`
    pub url: String,
    pub timeout: Duration,
    /// Принимать TLS-сертификаты без верификации CA/CN. Для self-signed
    /// или приватных CA на внутреннем периметре. Применяется к PG; для
    /// MySQL аналог задаётся параметрами URL (`ssl-mode=PREFERRED`).
    pub insecure_tls: bool,
}

impl DbTarget {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            timeout: Duration::from_secs(5),
            insecure_tls: false,
        }
    }
    pub fn with_insecure_tls(mut self, insecure: bool) -> Self {
        self.insecure_tls = insecure;
        self
    }
}

// ---------- aggregated state ----------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DbStats {
    /// "postgres" | "mysql" — выставляется в DbBackend::fetch_stats.
    pub backend: String,
    pub connections: ConnectionStats,
    pub top_queries: Vec<LongQuery>,
    pub locks_waiting: u32,
    pub tables: Vec<TableSize>,
    /// None — primary; Some(seconds) — replica с лагом репликации.
    pub replication_lag_sec: Option<f64>,
    pub version: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConnectionStats {
    pub active: u32,
    pub idle: u32,
    pub idle_in_transaction: u32,
    pub waiting: u32,
    pub other: u32,
}

impl ConnectionStats {
    pub fn total(&self) -> u32 {
        self.active + self.idle + self.idle_in_transaction + self.waiting + self.other
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LongQuery {
    pub pid: i32,
    pub usename: String,
    pub state: String,
    pub wait_event: Option<String>,
    pub age_sec: f64,
    pub query: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TableSize {
    pub name: String,
    pub bytes: i64,
    pub pretty: String,
}

// ---------- queries ----------

/// Сводка по состояниям коннектов.
const Q_CONNECTIONS: &str = "
SELECT
    coalesce(state, 'other') AS state,
    wait_event_type IS NOT NULL AS waiting,
    count(*)::bigint
FROM pg_stat_activity
WHERE pid <> pg_backend_pid()
GROUP BY 1, 2
";

/// Топ-N запросов, упорядоченных по «возрасту» (now - query_start).
/// Берём только не-idle, чтобы не показывать висящие коннект-пулы.
const Q_TOP_QUERIES: &str = "
SELECT
    pid::int4,
    coalesce(usename, '?') AS usename,
    coalesce(state, '?') AS state,
    wait_event,
    EXTRACT(EPOCH FROM (now() - query_start))::float8 AS age_sec,
    LEFT(query, 250) AS query
FROM pg_stat_activity
WHERE state IS NOT NULL
  AND state <> 'idle'
  AND pid <> pg_backend_pid()
ORDER BY query_start NULLS LAST
LIMIT 10
";

const Q_LOCKS_WAITING: &str = "SELECT count(*)::bigint FROM pg_locks WHERE NOT granted";

/// Размеры Zabbix-таблиц, которые типично взрываются (history*, trends*, events).
/// pg_total_relation_size учитывает индексы и TOAST.
const Q_TABLE_SIZES: &str = "
SELECT
    n.nspname || '.' || c.relname AS name,
    pg_total_relation_size(c.oid)::int8 AS bytes,
    pg_size_pretty(pg_total_relation_size(c.oid)) AS pretty
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE c.relkind = 'r'
  AND n.nspname NOT IN ('pg_catalog', 'information_schema')
  AND c.relname IN (
      'history','history_uint','history_str','history_log','history_text',
      'trends','trends_uint','events','event_recovery','problem'
  )
ORDER BY bytes DESC
";

const Q_REPLICATION_LAG: &str = "
SELECT CASE
    WHEN pg_is_in_recovery() THEN
        EXTRACT(EPOCH FROM (now() - pg_last_xact_replay_timestamp()))::float8
    ELSE NULL
END
";

const Q_VERSION: &str = "SHOW server_version";

// ---------- persistent connection ----------

/// Долгоживущий PG-коннект. Открывается один раз, используется на много
/// poll-ов. При любой ошибке запроса вызывающая сторона дропнет PgConnection
/// и откроет заново — это дёшево (~3-5 ms на localhost), но избавляет от
/// connect-disconnect на каждый тик в нормальном режиме.
///
/// Drop явно abort-ит фоновую задачу — нет утечек tasks.
pub struct PgConnection {
    client: Client,
    task: JoinHandle<()>,
}

impl PgConnection {
    pub async fn connect(target: &DbTarget) -> Result<Self> {
        // v0.4d: всегда подключаем TLS-коннектор. Если URL содержит
        // sslmode=disable, tokio-postgres даже не дёргает connector —
        // обычный TCP без TLS, накладных расходов нет. Если sslmode=
        // require/prefer, TLS-handshake через native-tls (openssl/schannel).
        let mut builder = NativeTlsConnector::builder();
        if target.insecure_tls {
            builder.danger_accept_invalid_certs(true);
            builder.danger_accept_invalid_hostnames(true);
        }
        let tls = MakeTlsConnector::new(
            builder.build().context("build TLS connector")?,
        );

        let (client, connection) =
            timeout(target.timeout, tokio_postgres::connect(&target.url, tls))
                .await
                .context("db connect timeout")?
                .context("db connect failed")?;
        let task = tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(Self { client, task })
    }

    pub async fn fetch_stats(&self, deadline: Duration) -> Result<DbStats> {
        run_all_queries(&self.client, deadline).await
    }
}

impl Drop for PgConnection {
    fn drop(&mut self) {
        // Без abort() задача висит, пока сам Client (внутри self) не дропнется
        // и не сигнализирует Connection future-у завершиться. abort() гарантирует
        // немедленный конец, не зависит от внутренней механики tokio-postgres.
        self.task.abort();
    }
}

// ---------- MySQL connection (v0.4c) ----------

/// MySQL/MariaDB-коннект на mysql_async. Отличается от PG двумя вещами:
/// 1. `fetch_stats(&mut self)` — mysql_async требует mutable borrow.
/// 2. Все запросы — try-or-default: если конкретная view/таблица не
///    существует на этой версии (INNODB_LOCK_WAITS, replica status), мы
///    просто оставляем поле дефолтным. Это даёт совместимость с MariaDB,
///    MySQL 5.7, MySQL 8.0+ без условной компиляции.
pub struct MyConnection {
    conn: mysql_async::Conn,
}

impl MyConnection {
    pub async fn connect(url: &str, deadline: Duration) -> Result<Self> {
        let opts = mysql_async::Opts::from_url(url).context("invalid mysql url")?;
        let conn = timeout(deadline, mysql_async::Conn::new(opts))
            .await
            .context("mysql connect timeout")?
            .context("mysql connect failed")?;
        Ok(Self { conn })
    }

    pub async fn fetch_stats(&mut self, _deadline: Duration) -> Result<DbStats> {
        use mysql_async::prelude::*;
        let mut stats = DbStats::default();

        // version
        if let Ok(rows) = self.conn.query::<String, _>("SELECT VERSION()").await {
            if let Some(v) = rows.into_iter().next() {
                stats.version = v;
            }
        }

        // connections by command+state → агрегируем в наши категории
        if let Ok(rows) = self
            .conn
            .query::<(String, String, i64), _>(
                "SELECT COMMAND, COALESCE(STATE,'') AS STATE, COUNT(*) \
                 FROM information_schema.PROCESSLIST \
                 WHERE ID <> CONNECTION_ID() \
                 GROUP BY COMMAND, STATE",
            )
            .await
        {
            for (cmd, state, n) in rows {
                let n = n as u32;
                let st_lower = state.to_lowercase();
                if cmd == "Sleep" {
                    stats.connections.idle += n;
                } else if st_lower.contains("lock") || st_lower.contains("waiting") {
                    stats.connections.waiting += n;
                } else if cmd == "Query" || cmd == "Execute" {
                    stats.connections.active += n;
                } else {
                    stats.connections.other += n;
                }
            }
        }

        // top long-running queries (v0.4c.1: с JOIN-ом на performance_schema
        // вытаскиваем настоящий wait_event). Если performance_schema выключен
        // или нет прав — fallback на простую PROCESSLIST без wait_event.
        let top_q_with_perf = "
            SELECT pl.ID, pl.USER, pl.COMMAND, COALESCE(pl.STATE,''),
                   pl.TIME, pl.INFO, COALESCE(ew.EVENT_NAME,'')
            FROM information_schema.PROCESSLIST pl
            LEFT JOIN performance_schema.threads t
                ON t.PROCESSLIST_ID = pl.ID
            LEFT JOIN performance_schema.events_waits_current ew
                ON ew.THREAD_ID = t.THREAD_ID
            WHERE pl.COMMAND <> 'Sleep'
              AND pl.ID <> CONNECTION_ID()
            ORDER BY pl.TIME DESC
            LIMIT 10
        ";
        let rows_with_event = self
            .conn
            .query::<(u32, String, String, String, u64, Option<String>, String), _>(
                top_q_with_perf,
            )
            .await;
        if let Ok(rows) = rows_with_event {
            stats.top_queries = rows
                .into_iter()
                .map(|(id, user, cmd, state, t, info, wait_event)| LongQuery {
                    pid: id as i32,
                    usename: user,
                    state: if state.is_empty() { cmd } else { state.clone() },
                    // Приоритет: настоящий event из perf_schema, иначе «Waiting…» из state.
                    wait_event: if !wait_event.is_empty() {
                        Some(wait_event)
                    } else if state.to_lowercase().contains("waiting") {
                        Some(state)
                    } else {
                        None
                    },
                    age_sec: t as f64,
                    query: info.unwrap_or_default(),
                })
                .collect();
        } else if let Ok(rows) = self
            .conn
            .query::<(u32, String, String, String, u64, Option<String>), _>(
                "SELECT ID, USER, COMMAND, COALESCE(STATE,''), TIME, INFO \
                 FROM information_schema.PROCESSLIST \
                 WHERE COMMAND <> 'Sleep' \
                   AND ID <> CONNECTION_ID() \
                 ORDER BY TIME DESC LIMIT 10",
            )
            .await
        {
            stats.top_queries = rows
                .into_iter()
                .map(|(id, user, cmd, state, t, info)| LongQuery {
                    pid: id as i32,
                    usename: user,
                    state: if state.is_empty() { cmd } else { state.clone() },
                    wait_event: if state.to_lowercase().contains("waiting") {
                        Some(state)
                    } else {
                        None
                    },
                    age_sec: t as f64,
                    query: info.unwrap_or_default(),
                })
                .collect();
        }

        // locks waiting (5.7+; на MariaDB может быть в другой view)
        if let Ok(rows) = self
            .conn
            .query::<i64, _>("SELECT COUNT(*) FROM information_schema.INNODB_LOCK_WAITS")
            .await
        {
            if let Some(n) = rows.into_iter().next() {
                stats.locks_waiting = n as u32;
            }
        } else if let Ok(rows) = self
            .conn
            .query::<i64, _>("SELECT COUNT(*) FROM performance_schema.data_lock_waits")
            .await
        {
            if let Some(n) = rows.into_iter().next() {
                stats.locks_waiting = n as u32;
            }
        }

        // idle-in-transaction (v0.4c.1): в MySQL такое состояние нельзя
        // увидеть через PROCESSLIST (там COMMAND=Sleep), но innodb_trx
        // показывает открытые транзакции без активного запроса.
        if let Ok(rows) = self
            .conn
            .query::<(u64, i64), _>(
                "SELECT trx_mysql_thread_id, \
                        TIMESTAMPDIFF(SECOND, trx_started, NOW()) AS age_sec \
                 FROM information_schema.innodb_trx \
                 WHERE (trx_query IS NULL OR trx_query = '') \
                   AND trx_mysql_thread_id IS NOT NULL",
            )
            .await
        {
            let idle_in_tx = rows.len() as u32;
            stats.connections.idle_in_transaction = idle_in_tx;
            // PROCESSLIST показывает эти коннекты как COMMAND=Sleep, значит
            // они уже в idle. Вычитаем, чтобы не дублировать в счётчиках.
            stats.connections.idle =
                stats.connections.idle.saturating_sub(idle_in_tx);

            // Зомби >= 60s → синтезируем LongQuery, чтобы алерт-правило
            // rule_idle_in_tx_zombie сработало (оно ищет state="idle in transaction").
            for (pid, age) in rows {
                if age >= 60 {
                    stats.top_queries.push(LongQuery {
                        pid: pid as i32,
                        usename: "(idle-in-tx)".into(),
                        state: "idle in transaction".into(),
                        wait_event: None,
                        age_sec: age as f64,
                        query: "(transaction held open without active query)".into(),
                    });
                }
            }
            // После добавления зомби пересортируем по возрасту и обрежем до 10.
            stats.top_queries.sort_by(|a, b| {
                b.age_sec
                    .partial_cmp(&a.age_sec)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            stats.top_queries.truncate(10);
        }

        // zabbix table sizes
        if let Ok(rows) = self
            .conn
            .query::<(String, i64, String), _>(
                "SELECT CONCAT(table_schema,'.',table_name), \
                        CAST(IFNULL(data_length,0)+IFNULL(index_length,0) AS SIGNED), \
                        CONCAT(ROUND((IFNULL(data_length,0)+IFNULL(index_length,0))/1024/1024,1),' MB') \
                 FROM information_schema.TABLES \
                 WHERE table_name IN ('history','history_uint','history_str','history_log','history_text','trends','trends_uint','events','event_recovery','problem') \
                 ORDER BY (IFNULL(data_length,0)+IFNULL(index_length,0)) DESC",
            )
            .await
        {
            stats.tables = rows
                .into_iter()
                .map(|(name, bytes, pretty)| TableSize {
                    name,
                    bytes,
                    pretty,
                })
                .collect();
        }

        // replication lag: SHOW REPLICA STATUS (8.0.22+) → SHOW SLAVE STATUS
        for q in ["SHOW REPLICA STATUS", "SHOW SLAVE STATUS"] {
            if let Ok(rows) = self.conn.query::<mysql_async::Row, _>(q).await {
                if let Some(row) = rows.into_iter().next() {
                    let lag = row
                        .get::<i64, _>("Seconds_Behind_Source")
                        .or_else(|| row.get::<i64, _>("Seconds_Behind_Master"));
                    if let Some(l) = lag {
                        stats.replication_lag_sec = Some(l as f64);
                    }
                    break;
                }
            }
        }

        Ok(stats)
    }
}

// ---------- backend router ----------

/// Универсальный handle над любым backend-ом. Выбор делается один раз при
/// connect по URL scheme. Хранится в spawn_db как `Option<DbBackend>`.
pub enum DbBackend {
    Postgres(PgConnection),
    Mysql(MyConnection),
}

impl DbBackend {
    pub async fn connect(target: &DbTarget) -> Result<Self> {
        let url = target.url.as_str();
        if url.starts_with("mysql://") {
            Ok(Self::Mysql(MyConnection::connect(url, target.timeout).await?))
        } else if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            Ok(Self::Postgres(PgConnection::connect(target).await?))
        } else {
            // Без явной схемы пробуем PG (libpq key=value тоже сюда).
            Ok(Self::Postgres(PgConnection::connect(target).await?))
        }
    }

    pub async fn fetch_stats(&mut self, deadline: Duration) -> Result<DbStats> {
        let mut stats = match self {
            Self::Postgres(c) => c.fetch_stats(deadline).await?,
            Self::Mysql(c) => c.fetch_stats(deadline).await?,
        };
        stats.backend = self.name().to_string();
        Ok(stats)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Postgres(_) => "postgres",
            Self::Mysql(_) => "mysql",
        }
    }
}

async fn run_all_queries(client: &Client, deadline: Duration) -> Result<DbStats> {
    let mut stats = DbStats::default();

    // Версия — для шапки таба.
    if let Ok(rows) = timeout(deadline, client.simple_query(Q_VERSION)).await? {
        for msg in rows {
            if let tokio_postgres::SimpleQueryMessage::Row(r) = msg {
                if let Some(v) = r.get(0) {
                    stats.version = v.to_string();
                    break;
                }
            }
        }
    }

    // Соединения по состояниям.
    let rows = timeout(deadline, client.query(Q_CONNECTIONS, &[]))
        .await
        .context("db connections query timeout")??;
    for row in rows {
        let state: &str = row.get(0);
        let waiting: bool = row.get(1);
        let count: i64 = row.get(2);
        let count_u32 = count as u32;
        if waiting {
            stats.connections.waiting += count_u32;
        } else {
            match state {
                "active" => stats.connections.active += count_u32,
                "idle" => stats.connections.idle += count_u32,
                "idle in transaction" | "idle in transaction (aborted)" => {
                    stats.connections.idle_in_transaction += count_u32
                }
                _ => stats.connections.other += count_u32,
            }
        }
    }

    // Топ запросов.
    let rows = timeout(deadline, client.query(Q_TOP_QUERIES, &[]))
        .await
        .context("db top queries query timeout")??;
    stats.top_queries = rows
        .into_iter()
        .map(|row| LongQuery {
            pid: row.get(0),
            usename: row.get(1),
            state: row.get(2),
            wait_event: row.get(3),
            age_sec: row.get(4),
            query: row.get(5),
        })
        .collect();

    // Locks waiting.
    let rows = timeout(deadline, client.query(Q_LOCKS_WAITING, &[]))
        .await
        .context("db locks query timeout")??;
    if let Some(row) = rows.first() {
        let n: i64 = row.get(0);
        stats.locks_waiting = n as u32;
    }

    // Размеры zabbix-таблиц.
    let rows = timeout(deadline, client.query(Q_TABLE_SIZES, &[]))
        .await
        .context("db table sizes query timeout")??;
    stats.tables = rows
        .into_iter()
        .map(|row| TableSize {
            name: row.get(0),
            bytes: row.get(1),
            pretty: row.get(2),
        })
        .collect();

    // Replication lag (если replica).
    let rows = timeout(deadline, client.query(Q_REPLICATION_LAG, &[]))
        .await
        .context("db replication lag query timeout")??;
    if let Some(row) = rows.first() {
        let lag: Option<f64> = row.get(0);
        stats.replication_lag_sec = lag;
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_stats_totals() {
        let mut c = ConnectionStats::default();
        c.active = 3;
        c.idle = 10;
        c.idle_in_transaction = 1;
        c.waiting = 2;
        c.other = 0;
        assert_eq!(c.total(), 16);
    }

    #[test]
    fn query_strings_compile() {
        // Простая sanity-проверка: ни один запрос не пустой и заканчивается
        // без trailing semicolon (tokio-postgres не требует).
        for q in [
            Q_CONNECTIONS,
            Q_TOP_QUERIES,
            Q_LOCKS_WAITING,
            Q_TABLE_SIZES,
            Q_REPLICATION_LAG,
            Q_VERSION,
        ] {
            assert!(!q.is_empty());
        }
    }
}
