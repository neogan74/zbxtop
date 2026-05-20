//! v0.4d.1 — обогащение DB-URL паролем из libpq-совместимых файлов.
//!
//! Идея: если в `--db-url` пароль не указан, пытаемся достать его из
//! стандартных мест: `~/.pgpass` для PG, `[client]` секции `~/.my.cnf`
//! для MySQL. Это библиотечное поведение — у PG то же самое делает libpq,
//! у MySQL — клиент `mysql` CLI.
//!
//! Если файл не существует, права слишком широкие, или нет матча — URL
//! возвращается как есть. Дальше уже сервер скажет про auth error.

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

// ---------- public API ----------

pub fn enrich_pg_url(url: &str) -> String {
    enrich(url, Backend::Postgres)
}

pub fn enrich_my_url(url: &str) -> String {
    enrich(url, Backend::Mysql)
}

#[derive(Clone, Copy)]
enum Backend {
    Postgres,
    Mysql,
}

fn enrich(url: &str, backend: Backend) -> String {
    let Some(mut parsed) = parse_db_url(url) else {
        return url.to_string();
    };
    if parsed.password.is_some() {
        return url.to_string(); // пароль уже в URL — ничего не делаем
    }
    let lookup = match backend {
        Backend::Postgres => read_pgpass_for(
            &parsed.host,
            parsed.port.unwrap_or(5432),
            &parsed.database,
            &parsed.user,
        ),
        Backend::Mysql => read_my_cnf_password(&parsed.user),
    };
    if let Some(pw) = lookup {
        parsed.password = Some(pw);
        parsed.to_url()
    } else {
        url.to_string()
    }
}

// ---------- URL parsing/rebuild ----------

#[derive(Debug, Clone)]
struct ParsedUrl {
    scheme: String,
    user: String,
    password: Option<String>,
    host: String,
    port: Option<u16>,
    database: String,
    /// Часть после "?" если есть.
    query: Option<String>,
}

impl ParsedUrl {
    fn to_url(&self) -> String {
        let mut s = String::with_capacity(64);
        s.push_str(&self.scheme);
        s.push_str("://");
        if !self.user.is_empty() {
            s.push_str(&self.user);
            if let Some(pw) = &self.password {
                s.push(':');
                s.push_str(&pct_encode(pw));
            }
            s.push('@');
        }
        s.push_str(&self.host);
        if let Some(p) = self.port {
            s.push(':');
            s.push_str(&p.to_string());
        }
        if !self.database.is_empty() {
            s.push('/');
            s.push_str(&self.database);
        }
        if let Some(q) = &self.query {
            s.push('?');
            s.push_str(q);
        }
        s
    }
}

fn parse_db_url(url: &str) -> Option<ParsedUrl> {
    let (scheme, rest) = url.split_once("://")?;
    // Делим на authority и path?query: ищем первый '/' или '?'.
    let (authority, tail) = match rest.find(|c| c == '/' || c == '?') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    let (user, password) = match userinfo {
        Some(ui) => match ui.split_once(':') {
            Some((u, p)) => (u.to_string(), Some(pct_decode(p))),
            None => (ui.to_string(), None),
        },
        None => (String::new(), None),
    };
    // host может быть в [::1]-форме (IPv6) — но для прод-инфры zabbix-серверов
    // это редко; в MVP не поддерживаем. Простой rsplit_once(':').
    let (host, port) = if hostport.starts_with('[') {
        // IPv6 в квадратных скобках: [::1]:5432
        if let Some(end) = hostport.find(']') {
            let h = &hostport[1..end];
            let port = hostport[end + 1..].trim_start_matches(':').parse::<u16>().ok();
            (h.to_string(), port)
        } else {
            (hostport.to_string(), None)
        }
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse::<u16>().ok()),
            None => (hostport.to_string(), None),
        }
    };

    let (database, query) = if let Some(q_idx) = tail.find('?') {
        let path = &tail[..q_idx];
        let q = &tail[q_idx + 1..];
        (path.trim_start_matches('/').to_string(), Some(q.to_string()))
    } else {
        (tail.trim_start_matches('/').to_string(), None)
    };

    Some(ParsedUrl {
        scheme: scheme.to_string(),
        user,
        password,
        host,
        port,
        database,
        query,
    })
}

// Минимальный URL percent decode/encode для userinfo (пароль).
fn pct_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

// ---------- ~/.pgpass ----------

fn pgpass_path() -> Option<PathBuf> {
    if let Ok(v) = env::var("PGPASSFILE") {
        return Some(PathBuf::from(v));
    }
    let home = env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".pgpass"))
}

#[cfg(unix)]
fn pgpass_mode_safe(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(path) {
        Ok(m) => {
            let mode = m.mode() & 0o777;
            // libpq требует не более 0600. 0400 тоже принимаем — read-only.
            mode == 0o600 || mode == 0o400
        }
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn pgpass_mode_safe(_: &Path) -> bool {
    true
}

fn read_pgpass_for(host: &str, port: u16, database: &str, user: &str) -> Option<String> {
    let path = pgpass_path()?;
    if !pgpass_mode_safe(&path) {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    for line in content.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = split_pgpass_line(line);
        if fields.len() != 5 {
            continue;
        }
        let (h, p, db, u, pw) = (&fields[0], &fields[1], &fields[2], &fields[3], &fields[4]);
        let h_ok = h == "*" || h == host || h == "localhost" && host == "127.0.0.1";
        let p_ok = p == "*" || p.parse::<u16>().map(|x| x == port).unwrap_or(false);
        let db_ok = db == "*" || db == database;
        let u_ok = u == "*" || u == user;
        if h_ok && p_ok && db_ok && u_ok {
            return Some(pw.clone());
        }
    }
    None
}

/// Разбить строку pgpass на 5 полей. Учитываем backslash-escape: `\:` и `\\`.
fn split_pgpass_line(line: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(5);
    let mut cur = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(&next) = chars.peek() {
                    cur.push(next);
                    chars.next();
                }
            }
            ':' => {
                out.push(std::mem::take(&mut cur));
            }
            other => cur.push(other),
        }
    }
    out.push(cur);
    out
}

// ---------- ~/.my.cnf ----------

fn my_cnf_path() -> Option<PathBuf> {
    let home = env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".my.cnf"))
}

/// Парсим только `[client]` секцию. Возвращаем пароль если есть.
/// Также сверяем user если он задан в my.cnf — если не совпадает с URL,
/// игнорируем (более безопасно).
fn read_my_cnf_password(url_user: &str) -> Option<String> {
    let path = my_cnf_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    let creds = parse_my_cnf(&content);
    let pw = creds.get("password")?;
    // Если в [client] явно указан user и он не совпадает с user из URL —
    // лучше не подставлять (могут быть разные креды).
    if let Some(user_in_cnf) = creds.get("user") {
        if !url_user.is_empty() && user_in_cnf != url_user {
            return None;
        }
    }
    Some(strip_quotes(pw))
}

fn parse_my_cnf(content: &str) -> HashMap<String, String> {
    let mut creds = HashMap::new();
    let mut in_client = false;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_client = matches!(&line[1..line.len() - 1], "client" | "mysql" | "client-server");
            continue;
        }
        if !in_client {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            creds.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    creds
}

fn strip_quotes(s: &str) -> String {
    let t = s.trim();
    if (t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')) {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_pg_url() {
        let u = parse_db_url("postgres://user:pass@host:5432/zabbix?sslmode=require")
            .expect("parse");
        assert_eq!(u.scheme, "postgres");
        assert_eq!(u.user, "user");
        assert_eq!(u.password.as_deref(), Some("pass"));
        assert_eq!(u.host, "host");
        assert_eq!(u.port, Some(5432));
        assert_eq!(u.database, "zabbix");
        assert_eq!(u.query.as_deref(), Some("sslmode=require"));
    }

    #[test]
    fn parses_url_without_password() {
        let u = parse_db_url("postgres://user@host/db").expect("parse");
        assert_eq!(u.user, "user");
        assert!(u.password.is_none());
        assert_eq!(u.database, "db");
    }

    #[test]
    fn rebuilds_url_with_password() {
        let mut u = parse_db_url("postgres://user@host:5432/db?sslmode=require").unwrap();
        u.password = Some("p@ss/word".to_string());
        let rebuilt = u.to_url();
        // p@ss/word → p%40ss%2Fword после percent-encoding
        assert!(rebuilt.contains("p%40ss%2Fword"));
        assert!(rebuilt.starts_with("postgres://user:"));
        assert!(rebuilt.ends_with("?sslmode=require"));
    }

    #[test]
    fn pgpass_splits_with_escape() {
        let parts = split_pgpass_line(r"host:5432:db:user:p\:assword");
        assert_eq!(parts, vec!["host", "5432", "db", "user", "p:assword"]);
        let parts = split_pgpass_line(r"\:weird:5432:*:*:pw");
        assert_eq!(parts, vec![":weird", "5432", "*", "*", "pw"]);
    }

    #[test]
    fn my_cnf_parses_client_section() {
        let content = "
[mysql]
prompt = mysql>

[client]
user = ztop_ro
password = 'sec:ret'
host = db.example.com
port = 3306

[mysqld]
bind-address = 0.0.0.0
";
        let creds = parse_my_cnf(content);
        assert_eq!(creds.get("user").map(|s| s.as_str()), Some("ztop_ro"));
        // strip_quotes применяется на верхнем уровне, не в parse_my_cnf
        let pw = strip_quotes(creds.get("password").unwrap());
        assert_eq!(pw, "sec:ret");
        assert_eq!(creds.get("host").map(|s| s.as_str()), Some("db.example.com"));
    }

    #[test]
    fn pct_encode_decode_roundtrip() {
        let original = "p@ss:word/with space";
        let encoded = pct_encode(original);
        let decoded = pct_decode(&encoded);
        assert_eq!(decoded, original);
    }

    #[test]
    fn enrich_passthrough_when_password_present() {
        let url = "postgres://user:pass@host/db";
        assert_eq!(enrich_pg_url(url), url);
    }
}
