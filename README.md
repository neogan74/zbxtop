# ztop

A console (TUI) monitor for the internal state of **zabbix_server** over SSH.
The goal is to observe Zabbix server processes and logs when the web UI/API
is slow or unavailable: data is collected directly from the OS (`ps`, `/proc`, log
tail) and via the runtime-control commands of `zabbix_server` itself.

> [Русская документация](docs/README.ru.md)

## What it shows

- **Header** — host-level metrics (load, mem, swap, uptime) and **per-source badges**:
  `[ps ✓] 1s ago, 23ms` for ps/sys/stats, `[log ●] streaming` for the log stream.
  Color indicates health: green = fresh, yellow = STALE (>10s without update), red = not responding.
  You can immediately see which collector is stuck.
- **Processes** — a table of aggregates by `zabbix_server` fork roles (poller,
  trapper, history syncer, preprocessing worker, lld manager, escalator, ...):
  count, total CPU% (from ps), RSS, **BusyPs%** (% of non-idle forks from proctitle),
  **BusyZbx%** (busy.avg from `zabbix.stats`), **Δ** — the difference.
  A large positive Δ in red = the server considers itself busy but ps doesn't see it
  → forks are waiting on lock/DB/IO.
- **Internals** — data directly from `zabbix_server` via the trapper port without
  PHP/Apache/DB: version, uptime, queue, vps, busy% per process type with
  avg/max/min, write/read/value cache utilization. This tab works even when the web UI is down.
- **Database** (PostgreSQL) — a third layer: connections by state (active/idle/
  idle-in-tx/waiting), top long-running queries with wait events, lock waiters,
  Zabbix table sizes (history/trends/events with ASCII bars), replication lag.
  Visible independently from the Zabbix server: PG can be lagging while the server still
  considers itself healthy.
- **Diagnoses** (strip above the body, visible on any tab) — **alert chord**:
  rules that rely on ≥2 sources simultaneously. Examples:
  "history syncer busy 95% + waiting INSERT on `history*` → history not landing in DB",
  "Δ busy(zbx) - busy(ps) = +50% → process is waiting, not computing",
  "idle-in-tx > 600s → housekeeper is blocked". Color-coded Critical / Warning / Info,
  shows up to 3 diagnoses at a time, the section disappears when everything is OK.
- **Graphs** — ASCII sparklines for the last ~8 minutes: total fork CPU,
  used memory, loadavg(1m).
- **Logs** — **real-time streaming** of `zabbix_server.log` via `tail -F` over
  a single long-lived SSH channel. Lines appear instantly, not in batches.
  Level highlighting (error/warning/info) and live filter. The header badge
  shows stream status (`●` streaming / `○` reconnecting ×N).
- **Runtime control** — hotkeys and modal menu for `zabbix_server -R` commands:
  log_level_increase/decrease, config_cache_reload, snmp_cache_reload,
  housekeeper_execute, diaginfo.

Zabbix API is **intentionally not used** — the tool must work when the DB/UI/API itself is slow.

## Local testbed (docker-compose)

Don't want to risk a production Zabbix? Bring up the full stack locally with
docker-compose — all four layers (OS / Zabbix / PG / probes) work end-to-end,
including stress scenarios that can trigger specific diagnose rules:

```bash
cd docker && ./setup.sh && docker compose up -d --build
cargo run --release -- --config docker/hosts.toml
```

Details: [docker/README.md](docker/README.md).

## Build

```bash
cargo build --release
```

The resulting binary is `target/release/ztop` (~3–5 MB, statically linked for most dependencies).

## Running

**Single-host (CLI):**

```bash
ztop --host zbx-prod-01 \
     --log  /var/log/zabbix/zabbix_server.log \
     --sudo                 # optional, see below
     --stats-port 10051     # default
     # --no-stats           # disable trapper source if unavailable
```

All flags are also available as environment variables: `ZTOP_HOST`, `ZTOP_LOG`,
`ZTOP_SUDO`, `ZTOP_STATS_HOST`, `ZTOP_STATS_PORT`, `ZTOP_NO_STATS`,
`ZTOP_CONFIG`, `ZTOP_DB_URL`, `ZTOP_DB_INSECURE_TLS`.

**Multi-host via config (v0.5a):**

Create `~/.config/ztop/hosts.toml` (or `--config <path>`):

```toml
[[host]]
name = "prod"
ssh = "zbx-prod-01"
log = "/var/log/zabbix/zabbix_server.log"
sudo = true
db_url = "postgres://ztop_ro:secret@db-prod/zabbix?sslmode=require"

[[host]]
name = "staging"
ssh = "ztop@zbx-stage.internal"
no_stats = true     # trapper is behind firewall

[[host]]
name = "dev"
ssh = "zbx-dev"
log = "/srv/zabbix/log/server.log"
db_url = "mysql://ztop_ro:secret@db-dev/zabbix"
```

Just run `ztop` — the config is picked up from the default path. The UI will show
a **`0 Overview`** tab with a health indicator for all hosts, and a
**Fleet aggregate** below the table (total CPU/queue, max load, mem avg/max, total
diagnosis count with critical highlights).

Switching between hosts:

- **Ctrl-N / Ctrl-P** — from any tab, sequential cycling.
- On Overview — `↑`/`↓` (or `j`/`k`) move the **cursor** (`▸`), **Enter**
  sets the focused host (`●`) and immediately switches to `1 Processes`.
- **Ctrl-G** (v0.5c) — modal host picker with **fuzzy search** by name and
  SSH target. Type a few letters to narrow the list. Useful with 20+ hosts
  where Ctrl-N/P cycling is slow. For example, `prod` will find all production
  hosts with a consecutive-match bonus.

Diagnoses accumulate across the fleet with a `[hostname]` prefix; the top-3
most-severe are shown above the body regardless of the current tab.

**zabbix_proxy** is supported (v0.5b): proxy-specific roles
(`data sender`, `heartbeat sender`, `vmware collector`) are recognized
automatically. In `hosts.toml` the configuration is the same for server and proxy.

## Synthetic probes (v0.7)

ztop can run **lightweight probes from the machine where it is running**: TCP-connect,
DNS-resolve, PG `SELECT 1`. The goal is to immediately see whether latency is
**on my laptop/network** or the server/DB is genuinely sick. This is the fourth
"resilience layer" — the closest to the human, requires no access to the remote host.

Add `[[probe]]` blocks to the same `hosts.toml`:

```toml
[[probe]]
name = "DB primary TCP"
kind = "tcp"
target = "db-prod-01:5432"
interval = 5      # default
timeout = 5

[[probe]]
name = "DNS internal LB"
kind = "dns"
target = "zbx-lb.internal"

[[probe]]
name = "PG latency"
kind = "pg"
url = "postgres://probe_ro:secret@db-prod-01/zabbix"
```

Tab `6 Probes` shows a table: name / kind / target / latency / status /
success% / last error. Latency color scale: green <100ms, yellow <1s, red >1s.
Passwords in URLs are masked as `****` for safe display.

Probes are **opt-in** — without `[[probe]]` blocks nothing is pinged
(to avoid accidentally DDoSing production). PG probes use the same password
reading logic from `.pgpass` as the main DB source.

## Record and replay (v0.6)

For post-mortem analysis — record all incoming telemetry to a file,
then replay it as a live session.

**Record an incident:**

```bash
ztop --record incident-2026-05-13.jsonl
```

The header will show `● REC (N ev)` — a counter of recorded events. The file is
JSON Lines, with a header on the first line containing the host list, followed by each
collector message with a timestamp.

**Replay a recording:**

```bash
ztop --replay incident-2026-05-13.jsonl
ztop --replay incident.jsonl --replay-speed 10    # 10x faster
```

Hosts are restored from the header — `--host`/`--config` are not needed.
The header shows `▶ REPLAY 45.2%` (progress). Runtime control (`R`, `+/-`, `c`,
`h`, `d`) is disabled; no SSH connections are opened. Everything else works:
tab switching, host focus, log filters, and the **diagnose alert chord** —
it is built on top of state and fires exactly as it did during recording.
You can scrub through the incident and see when each diagnosis lit up.

**Replay controls (v0.6.1):**

| Key          | Action                                                            |
|--------------|-------------------------------------------------------------------|
| `Space`      | Pause / resume                                                    |
| `n`          | Step — one event forward (while paused)                           |
| `>` or `.`   | Seek 60 seconds forward                                           |
| `<` or `,`   | Seek 60 seconds backward (resets state, rebuilds from new events) |

When seeking backward, host state is **reset** via a synthetic
`CollectorMsg::Reset` — otherwise the screen would show data from the "future"
relative to the new cursor position. State is then rebuilt naturally from events.

**File format** (JSON Lines):

```json
{"kind":"header","version":"0.6","started_at":"2026-05-13T03:00:00Z","hosts":[...]}
{"kind":"event","ts":"2026-05-13T03:14:23.123Z","host_idx":0,"msg":{"type":"Procs","result":{"Ok":[...]},"took_ms":23}}
{"kind":"event","ts":"2026-05-13T03:14:23.450Z","host_idx":0,"msg":{"type":"LogStreamLine","raw":"...","level":"Error"}}
```

Size: with a 3-host setup and moderate activity — ~10–50 KB/sec,
~36–180 MB for an hour-long incident. Compaction is deferred to v0.6.1.

## Configuring zabbix.stats (TCP 10051)

`zabbix_server.conf` must allow the IP from which ztop connects:

```ini
StatsAllowedIP=10.0.0.1,192.168.0.0/24
# or just 127.0.0.1 if ztop runs on the same machine
```

This is the **binary Zabbix protocol** on the trapper port, not the HTTP API.
No tokens — IP-ACL only. Works as long as the `zabbix_server` process is alive,
even if PHP/Apache/DB are down.

Manual verification: `echo '{"request":"zabbix.stats"}' | nc <host> 10051`
(the response has a 13-byte ZBXD header followed by JSON).

## DB scraper (PostgreSQL + MySQL/MariaDB)

ztop connects to the database directly and runs 6 queries: connections, top long
queries, lock waiters, Zabbix table sizes, replication lag, version.

The backend is selected by URL scheme:

- `postgres://...` or `postgresql://...` → PostgreSQL (`tokio-postgres`)
- `mysql://...` → MySQL/MariaDB (`mysql_async`)
- no scheme → default PG (libpq key=value also works here)

**A read-only role is required.** To create it on PG:

```sql
CREATE ROLE ztop_ro WITH LOGIN PASSWORD '<...>';
GRANT pg_read_all_stats TO ztop_ro;       -- pg_stat_activity, pg_locks, etc.
GRANT CONNECT ON DATABASE zabbix TO ztop_ro;
GRANT USAGE  ON SCHEMA public TO ztop_ro;
-- For table sizes (metadata read only):
GRANT SELECT ON pg_catalog.pg_class      TO ztop_ro;
GRANT SELECT ON pg_catalog.pg_namespace  TO ztop_ro;
```

Running:

```bash
ztop --host zbx-prod-01 \
     --db-url 'postgres://ztop_ro:<pass>@db-host:5432/zabbix?sslmode=disable'
```

The password in the URL is visible in `ps`/`/proc`. On production it is better to use an env var:

```bash
export ZTOP_DB_URL='postgres://ztop_ro:<pass>@db-host:5432/zabbix'
ztop --host zbx-prod-01
```

If PG is unavailable (firewall, no credentials) — just omit `--db-url`;
the DB tab will show a hint and all other sources continue as before.

**MySQL/MariaDB read-only setup:**

```sql
CREATE USER 'ztop_ro'@'%' IDENTIFIED BY '<...>';
GRANT SELECT, PROCESS, REPLICATION CLIENT, REPLICATION SLAVE ADMIN ON *.* TO 'ztop_ro'@'%';
-- PROCESS is needed for information_schema.PROCESSLIST and .innodb_trx
-- REPLICATION CLIENT — for SHOW REPLICA STATUS
-- SELECT on information_schema and performance_schema is granted implicitly
FLUSH PRIVILEGES;
```

`wait_event` requires `performance_schema` enabled (usually on by default
on MySQL 5.6+ and MariaDB 10.5+). If disabled or SELECT rights are missing, ztop
automatically falls back to a simplified PROCESSLIST without `wait_event`.

Running with MySQL:

```bash
ztop --host zbx-prod-01 \
     --db-url 'mysql://ztop_ro:<pass>@db-host:3306/zabbix?ssl-mode=REQUIRED'
```

## TLS to the database (v0.4d)

**PostgreSQL.** The TLS connector is always present via `postgres-native-tls`;
behavior is controlled by the `sslmode=` URL parameter:

- `sslmode=disable` — plain TCP (default for URLs without explicit mode).
- `sslmode=prefer` — TLS if the server supports it, otherwise plain (libpq default).
- `sslmode=require` — TLS mandatory, no CA verification required.
- `sslmode=verify-ca`/`verify-full` — TLS + full verification.

For self-signed certificates or private CAs on an internal perimeter,
use `--db-insecure-tls` (env `ZTOP_DB_INSECURE_TLS=true`): accepts any cert
without verification. **Do not use in production over WAN.**

**MySQL.** TLS is controlled via URL parameters through mysql_async:

- `?ssl-mode=DISABLED` — no TLS
- `?ssl-mode=REQUIRED` — TLS mandatory, no CA verification
- `?ssl-mode=VERIFY_CA`/`VERIFY_IDENTITY` — full verification

## Passwords from ~/.pgpass and ~/.my.cnf (v0.4d.1)

If the password is omitted from `--db-url` (`postgres://user@host/db`), ztop
reads the password from standard libpq-/mysql-compatible files.

**PostgreSQL — `~/.pgpass`** (or `$PGPASSFILE`):

```text
# host:port:database:user:password
db-host:5432:zabbix:ztop_ro:s3cret
*:5432:*:ztop_ro:fallback-password
```

libpq rules: the file must have `0600` (or `0400`) permissions, otherwise it is ignored.
`*` is a wildcard. Backslash-escape `:` and `\` in fields.

**MySQL — `~/.my.cnf`** `[client]` section:

```ini
[client]
user = ztop_ro
password = "s3:cret"
host = db-host
port = 3306
```

If `[client]` specifies a `user` that does not match the URL user, the password
is not substituted (protection against accidentally using another identity).

After enrichment the URL is passed to the driver normally. If the password cannot
be read from the file (missing file, wrong permissions, no match) — the URL is sent
as-is and the server will return an auth error.

## SSH configuration

The prototype calls the system `ssh` — which means everything that already works in
`~/.ssh/config` works here too: host aliases, ProxyJump, keys, agent.

It is recommended to add multiplexing to `~/.ssh/config` so repeated commands
share one TCP socket (snappy refresh):

```sshconfig
Host zbx-prod-01
    HostName zbx-prod-01.internal
    User      ztop
    ControlMaster auto
    ControlPath   /tmp/ztop-%r@%h:%p
    ControlPersist 60s
```

ztop passes these options itself, but an explicit entry in the config is convenient for debugging.

## Runtime control permissions

`zabbix_server -R` works for any user who can write to the server's runtime socket —
typically `zabbix` or `root`. Options:

1. **SSH login as the zabbix user** — simplest option, `--sudo` is not needed.
2. **Login as a regular user + `--sudo`** — add a sudoers entry:

   ```text
   ztopuser ALL=(root) NOPASSWD: /usr/sbin/zabbix_server -R *
   ```

   ztop calls `sudo -n` (passwordless), so NOPASSWD is required.

For viewing logs and `ps`/`/proc`, sudo is usually not needed.

## Structure

```text
src/
  main.rs          — CLI, terminal setup, channel-based event loop
  app.rs           — state, ring buffers for history, apply_msg
  source.rs        — per-collector tokio tasks with backoff + shared Notify
  ssh.rs           — wrapper around system ssh (tokio::process)
  collectors.rs    — parsers for ps/loadavg/meminfo/uptime/logs + runtime control
  zbxstats.rs      — binary Zabbix protocol: framing + fetch_stats (v0.3)
  db.rs            — PG + MySQL backends: DbBackend enum, TLS (v0.4/v0.4b-d)
  dbcreds.rs       — pgpass / my.cnf password reading (v0.4d.1)
  diagnose.rs      — cross-source rules: signal intersections (v0.4b)
  hosts.rs         — TOML config for multi-host (v0.5a)
  record.rs        — record and replay telemetry as JSONL (v0.6/v0.6.1)
  probes.rs        — synthetic TCP/DNS/PG probes (v0.7)
  ui/              — ratatui rendering: chrome (header/tabs/footer), one module
                     per tab, modals, shared helpers
```

**Polling architecture (v0.2a+b).** Each collector lives in its own
`tokio::spawn` task and sends results to a shared `mpsc` channel.
The event loop updates state on incoming messages and also redraws the screen
on a UI ticker (for toast decay and STALE timer recalculation).

- **Procs/Sys (periodic)** — every 2 seconds, runs `ssh ... 'cmd'`; errors trigger
  exponential backoff (capped at 30s).
- **Logs (streaming, v0.2b)** — one long-lived SSH process with `tail -n N -F`,
  delivers lines to the channel one at a time. Reconnects with backoff on EOF/error.
- **Force-refresh `r`** — a shared `Notify` wakes all periodic tasks immediately.
- **Force-reconnect `L`** — a separate Notify to restart the log stream.

Unit tests for parsers: `cargo test`.

## Post-MVP plans

1. **Log stream over one long SSH session** (`russh` + `tail -F`) to avoid polling.
2. **Diaginfo → structured blocks**: run `zabbix_server -R diaginfo`, wait for the
   log entry, parse historycache / valuecache / preprocessing sections and show
   them in a dedicated panel.
3. **Multi-host** — a separate tab with multiple Zabbix servers.
4. **Queue** — `zabbix_get`/SQL/`zabbix_server -R diaginfo` for queue size by prefer
   zones, separate graph.
5. **On-disk result caching** for post-mortem (CSV/JSONL `--record`).
6. **Proxy forks** — extend the parser for `zabbix_proxy:` (same proctitle format).
7. **TLS-status/DB latency** — simple synthetic probes (`time psql -c ...`, `nc -z`)
   to test the hypothesis "the DB is slow".
