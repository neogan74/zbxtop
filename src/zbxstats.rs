//! Второй транспорт: бинарный Zabbix-протокол на trapper-порту (TCP 10051).
//!
//! Это **не** HTTP API: PHP/Apache/БД не задействованы. Запрос отправляется
//! напрямую процессу `zabbix_server`, который сам ведёт self-monitoring и
//! отдаёт богатую внутреннюю телеметрию: busy% по типам процессов, кэши,
//! очередь, vps.
//!
//! Авторизация — IP-ACL через `StatsAllowedIP=<ip>` в `zabbix_server.conf`.
//! Никаких токенов; в этом плюс — нечему истечь в инциденте.
//!
//! Формат пакета (header 13 байт + JSON):
//!   "ZBXD" (4)  | flags (1, обычно 0x01) | datalen LE u32 (4) | reserved (4)
//! Старый формат до 4.0 читает байты 5..13 как u64 LE длину — но если данные
//! < 4 ГиБ и старшие 4 байта нулевые, обе схемы совместимы. Поэтому шлём
//! "ZBXD\x01" + datalen_u32_le + 0000_0000 — работает и со старыми и с
//! новыми серверами.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

#[derive(Clone, Debug)]
pub struct ZbxStatsTarget {
    pub host: String,
    pub port: u16,
    pub timeout: Duration,
}

impl ZbxStatsTarget {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            timeout: Duration::from_secs(5),
        }
    }
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

// ---------- protocol framing ----------

const MAGIC: &[u8; 4] = b"ZBXD";
const FLAG_UNCOMPRESSED: u8 = 0x01;
const FLAG_COMPRESSED: u8 = 0x02;
const HEADER_LEN: usize = 13;
const MAX_RESPONSE: usize = 16 * 1024 * 1024; // sanity cap, 16 MiB

/// Собрать пакет: header + JSON payload.
fn frame(payload: &[u8]) -> Vec<u8> {
    let datalen = payload.len() as u32;
    let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
    buf.extend_from_slice(MAGIC);
    buf.push(FLAG_UNCOMPRESSED);
    buf.extend_from_slice(&datalen.to_le_bytes());
    buf.extend_from_slice(&[0u8; 4]); // reserved
    buf.extend_from_slice(payload);
    buf
}

/// Прочитать ответ: 13-байтовый header + body длины из заголовка.
async fn read_response(stream: &mut TcpStream, deadline: Duration) -> Result<Vec<u8>> {
    let mut header = [0u8; HEADER_LEN];
    timeout(deadline, stream.read_exact(&mut header))
        .await
        .context("response header read timeout")??;

    if &header[0..4] != MAGIC {
        return Err(anyhow!(
            "invalid zabbix magic: expected ZBXD, got {:?}",
            &header[0..4]
        ));
    }
    let flags = header[4];
    if flags & FLAG_COMPRESSED != 0 {
        // Поддержку компрессии добавим, когда понадобится — большинство ответов
        // на zabbix.stats умещаются в килобайт-другой, smysl-а сжимать нет.
        return Err(anyhow!("compressed response not supported yet"));
    }
    let datalen = u32::from_le_bytes([header[5], header[6], header[7], header[8]]) as usize;
    if datalen > MAX_RESPONSE {
        return Err(anyhow!(
            "response too large: {} bytes (cap {})",
            datalen,
            MAX_RESPONSE
        ));
    }
    let mut body = vec![0u8; datalen];
    timeout(deadline, stream.read_exact(&mut body))
        .await
        .context("response body read timeout")??;
    Ok(body)
}

// ---------- public API ----------

pub async fn fetch_stats(target: &ZbxStatsTarget) -> Result<ZabbixStats> {
    let request = br#"{"request":"zabbix.stats"}"#;
    let pkt = frame(request);

    let mut stream = timeout(target.timeout, TcpStream::connect(target.addr()))
        .await
        .with_context(|| format!("connect timeout: {}", target.addr()))?
        .with_context(|| format!("connect failed: {}", target.addr()))?;

    // Уменьшаем TCP-задержку — пакет маленький, один write.
    stream.set_nodelay(true).ok();

    timeout(target.timeout, stream.write_all(&pkt))
        .await
        .context("write timeout")??;

    let body = read_response(&mut stream, target.timeout).await?;

    let resp: StatsResponse = serde_json::from_slice(&body).with_context(|| {
        format!(
            "parse stats JSON ({}): {}",
            body.len(),
            String::from_utf8_lossy(&body).chars().take(200).collect::<String>()
        )
    })?;

    if resp.response != "success" {
        return Err(anyhow!(
            "zabbix.stats returned non-success: {}",
            String::from_utf8_lossy(&body)
        ));
    }
    Ok(resp.data)
}

// ---------- response types ----------

/// Внешняя обёртка ответа: {"response": "success", "data": {...}}.
#[derive(Debug, Deserialize)]
struct StatsResponse {
    response: String,
    #[serde(default)]
    data: ZabbixStats,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ZabbixStats {
    #[serde(default)]
    pub version: String,
    /// uptime сервера в секундах (приходит строкой в старых версиях, числом в новых).
    #[serde(default, deserialize_with = "de_string_or_number")]
    pub uptime: u64,
    #[serde(default)]
    pub hostname: String,
    /// Размер очереди в форме, в которой её отдал сервер. Может быть числом
    /// (старый формат) или объектом с буферами. Храним как Value для гибкости
    /// между версиями.
    #[serde(default)]
    pub queue: serde_json::Value,
    /// `process.<type>` — busy% и count для каждого типа процесса.
    #[serde(default)]
    pub process: BTreeMap<String, ProcessStats>,
    #[serde(default)]
    pub wcache: serde_json::Value,
    #[serde(default)]
    pub rcache: serde_json::Value,
    #[serde(default)]
    pub vcache: serde_json::Value,
    #[serde(default)]
    pub vps: serde_json::Value,
}

impl ZabbixStats {
    /// Извлекаем queue как одно число (если оно числовое) либо None.
    pub fn queue_total(&self) -> Option<u64> {
        self.queue.as_u64()
    }
    /// Утилизация cache — pfree (свободные %) для любого из caches.
    /// Возвращает 100 - pfree, то есть «занято %».
    pub fn cache_used_pct(&self, name: CacheName) -> Option<f32> {
        let root = match name {
            CacheName::Write => &self.wcache,
            CacheName::Read => &self.rcache,
            CacheName::Value => &self.vcache,
        };
        // wcache может быть либо {"history": {"pfree":...}}, либо плоско {"pfree":...},
        // а у vcache часто отдельный {"buffer":{"pfree":...}}. Пробуем оба пути.
        let pfree = root
            .get("buffer")
            .and_then(|v| v.get("pfree"))
            .or_else(|| root.get("history").and_then(|v| v.get("pfree")))
            .or_else(|| root.get("pfree"))
            .and_then(|v| v.as_f64())?;
        Some(100.0 - pfree as f32)
    }
    /// Записей в секунду (total). У старых версий поле может отсутствовать.
    pub fn vps_total(&self) -> Option<f64> {
        self.vps
            .get("total")
            .and_then(|v| v.as_f64())
            .or_else(|| self.vps.as_f64())
    }
}

#[derive(Clone, Copy)]
pub enum CacheName {
    Write,
    Read,
    Value,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ProcessStats {
    #[serde(default)]
    pub busy: BusyStats,
    #[serde(default)]
    pub count: u32,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct BusyStats {
    #[serde(default)]
    pub avg: f32,
    #[serde(default)]
    pub max: f32,
    #[serde(default)]
    pub min: f32,
}

/// Десериализация числа, которое может прилететь как String или как number.
fn de_string_or_number<'de, D>(d: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_f64().map(|f| f as u64))
            .ok_or_else(|| D::Error::custom("not a u64")),
        serde_json::Value::String(s) => s.parse::<u64>().map_err(D::Error::custom),
        serde_json::Value::Null => Ok(0),
        other => Err(D::Error::custom(format!(
            "expected number or string, got {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_is_well_formed() {
        let f = frame(b"hi");
        assert_eq!(&f[0..4], b"ZBXD");
        assert_eq!(f[4], 0x01);
        assert_eq!(&f[5..9], &2u32.to_le_bytes());
        assert_eq!(&f[9..13], &[0, 0, 0, 0]);
        assert_eq!(&f[13..], b"hi");
        assert_eq!(f.len(), HEADER_LEN + 2);
    }

    /// Реалистичный sample ответа zabbix.stats (Zabbix 6.0).
    const SAMPLE: &str = r#"{
        "response": "success",
        "data": {
            "version": "6.0.20",
            "uptime": "12345",
            "boottime": "1700000000",
            "hostname": "zbx-prod-01",
            "process": {
                "poller": {
                    "busy": {"avg": 12.3, "max": 25.5, "min": 0.0},
                    "count": 5
                },
                "history syncer": {
                    "busy": {"avg": 75.0, "max": 92.1, "min": 60.0},
                    "count": 4
                },
                "trapper": {
                    "busy": {"avg": 3.2, "max": 8.0, "min": 0.0},
                    "count": 8
                }
            },
            "queue": 234,
            "wcache": {
                "values": 1234567,
                "history": {"pfree": 95.5, "free": 1024, "total": 1048576}
            },
            "rcache": {
                "buffer": {"pfree": 99.0, "free": 1024, "total": 1024}
            },
            "vcache": {
                "buffer": {"pfree": 80.0},
                "cache": {"hits": 1234, "requests": 1500, "misses": 266}
            },
            "vps": {"total": 1234.5, "written": 1230.1}
        }
    }"#;

    #[test]
    fn parses_realistic_payload() {
        let resp: StatsResponse = serde_json::from_str(SAMPLE).expect("parse");
        assert_eq!(resp.response, "success");
        let d = resp.data;
        assert_eq!(d.version, "6.0.20");
        assert_eq!(d.uptime, 12345);
        assert_eq!(d.hostname, "zbx-prod-01");
        assert_eq!(d.process.len(), 3);
        let hs = d.process.get("history syncer").expect("history syncer");
        assert_eq!(hs.count, 4);
        assert!((hs.busy.avg - 75.0).abs() < 1e-3);
        assert_eq!(d.queue_total(), Some(234));
        assert!((d.cache_used_pct(CacheName::Read).unwrap() - 1.0).abs() < 1e-2);
        assert!((d.cache_used_pct(CacheName::Value).unwrap() - 20.0).abs() < 1e-2);
        assert!((d.cache_used_pct(CacheName::Write).unwrap() - 4.5).abs() < 1e-2);
        assert!((d.vps_total().unwrap() - 1234.5).abs() < 1e-3);
    }

    #[test]
    fn uptime_accepts_number_form() {
        // Новые версии шлют uptime числом, а не строкой.
        let j = r#"{"response":"success","data":{"uptime":42}}"#;
        let resp: StatsResponse = serde_json::from_str(j).expect("parse");
        assert_eq!(resp.data.uptime, 42);
    }

    #[test]
    fn graceful_on_minimal_data() {
        let j = r#"{"response":"success","data":{}}"#;
        let resp: StatsResponse = serde_json::from_str(j).expect("parse");
        assert_eq!(resp.data.uptime, 0);
        assert!(resp.data.process.is_empty());
        assert!(resp.data.queue_total().is_none());
    }
}
