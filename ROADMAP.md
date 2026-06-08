# ztop roadmap

Sorting principle — **"works on a sick system"**. Each wave adds value precisely
when the Zabbix server is in bad shape: slow, flooding logs, UI not responding.
Pure cosmetics, extra tabs, and themes are at the very bottom.

> [Русская документация](docs/ROADMAP.ru.md)

## Architectural idea: layered data access

ztop collects data from several sources with different "resilience" levels. The more
sources are connected, the more precisely ztop can tell **what exactly broke**.
Each layer fails earlier in an incident than the one before it:

| Source                         | Alive while...                      | Implementation |
|--------------------------------|-------------------------------------|----------------|
| OS (SSH → `ps`, `/proc`, log)  | the machine itself is alive         | v0.1 (done) |
| `zabbix.stats` TCP:10051       | the `zabbix_server` process is alive | v0.3 |
| DB direct (`pg_stat_*`, MySQL) | the DB is alive (even if server is dead) | v0.4 |
| HTTP API                       | PHP + Apache + DB are all alive     | **intentionally skipped** |

One screen in the UI — multiple panels marked with the source: the user immediately sees
which ones are "silent" and understands at which layer the failure occurred.

---

## v0.1 — MVP (initial iteration)

- [x] Processes tab: aggregate by roles from `ps`/proctitle
- [x] Graphs tab: sparklines for CPU sum / mem% / load1
- [x] Logs tab: tail with level highlighting + live filter
- [x] Runtime control: hotkeys + modal menu
- [x] SSH via system `ssh` with ControlMaster
- [x] Unit tests for parsers

---

## v0.2a — resilience, channel-based event loop

Goal: **never hang and never silently show stale data**.

- [x] **Background refresh tasks** (separate tokio tasks per collector, channel
  into render loop) so a slow collector doesn't block fast ones. Implemented
  in `src/source.rs` (spawn_collectors + three parallel tasks).
- [x] **Backoff and retry** on SSH network errors: exponential, capped at 30s,
  reset on success. Fixed by unit tests.
- [x] **Per-source badges in the header**: `[ps ✓] 1s ago, 23ms`, `[sys ⚠] 14s ago,
  4200ms err×3`. Color — green/yellow/red based on STALE/ERROR.
- [x] **STALE indicator** (10 seconds without a successful update).
- [x] **Force-refresh `r`** converted to a Notify to all collectors.
- [x] **Pause `p`**: the receiver drops incoming messages, collectors keep running —
  after unpause the picture is immediately fresh, not delayed by a full interval.

## v0.2b — streaming logs via tail -F

Goal: **log lines appear instantly**, without polling. The main visible
benefit during an incident.

- [x] **`tail -n N -F` via a long-lived SSH subprocess**, reading stdout
  line-by-line via `tokio::io::BufReader::lines()` → `mpsc<CollectorMsg::LogStreamLine>`.
- [x] **Reconnect with backoff** on EOF/error (exponential, capped at 30s).
- [x] **Header badge**: `[log ●] streaming, last 3s ago, reconn ×2` —
  separate semantics: "no new lines" (normal) vs. "stream dropped".
- [x] **Force-reconnect `L`**: manual trigger via `Notify`, no waiting for backoff.
- [x] **Log buffer cap** at MAX_LOG_LINES (5000) with FIFO trimming.

## v0.2c — russh long-lived session (deferred)

`russh` (pure-Rust SSH) gives ~5 ms latency per exec vs. ~50–200 ms for process-ssh.
**However**, with ControlMaster (already enabled in v0.1) process-ssh latency drops
to 5–15 ms via unix-socket multiplexing — marginal win. We'll revisit only if
profiling shows fork/exec ssh is a real bottleneck.

When we get to it, scope:
- [ ] Transport abstraction (enum `Transport::Process | Russh`).
- [ ] Auth: SSH agent (via SSH_AUTH_SOCK) + identity file.
- [ ] Known hosts: first iteration — `--insecure-host-key`, then parsing `~/.ssh/known_hosts`.
- [ ] Health metric "session up Xs, channels N" in the header.

## v0.3 — `zabbix.stats` via trapper port ✅

Goal: **get rich internal telemetry without SSH dependency** —
the same channel that Zabbix uses for self-monitoring.

This is a request using the binary Zabbix protocol on port **TCP:10051**. This is **not**
the HTTP API: PHP/Apache/DB are not involved, only the `zabbix_server` process itself.
Authorization — IP-ACL via `StatsAllowedIP` in `zabbix_server.conf` (no token).
Client-side — TCP + Zabbix header (`ZBXD` + flags + length) + JSON.

- [x] New module **`zbxstats.rs`**: framing (ZBXD\x01 + u32 LE +
  reserved), TCP to the trapper port, parser. Unit tests on realistic
  payload + minimal case.
- [x] Request `{"request":"zabbix.stats"}` — version/uptime/hostname,
  process busy% (avg/max/min) by type, wcache/rcache/vcache utilization,
  queue, vps. queue/vps stored as `serde_json::Value` for flexibility
  across Zabbix versions.
- [x] New panel **"Internals" (tab 4)**: server identity, queue, vps,
  process table with busy avg/max/min, gauge caches.
- [x] **Cross-compare** in the Processes tab: new columns `BusyPs`,
  `BusyZbx`, `Δ` — the discrepancy (high busy from stats, low CPU/ps)
  is a clear sign of waiting (lock/DB/IO), highlighted in red.
- [x] **Source indicator** in the header: `[stats ✓] 1s ago, 45ms` alongside
  other sources.
- [x] CLI: `--stats-host`, `--stats-port` (default 10051), `--no-stats`.

Deferred to v0.3b:
- [ ] Request `{"request":"zabbix.stats","type":"queue","params":{...}}` —
  queue histogram by delay buckets.
- [ ] TLS PSK/cert on the trapper port (for setups with a secured trapper).
- [ ] Keep-alive: currently each poll opens a new TCP connection (latency ~5–30 ms
  in a datacenter). If this becomes a bottleneck — switch to socket reuse.

**Boundary with v0.2 `diaginfo`:** some data overlaps. If the stats endpoint
is available, `diaginfo` is only needed for top-N items in historycache/valuecache
that are not in `stats`. Logic:
- if `stats` responds — take cache/queue from there (cheap, synchronous);
- `zabbix_server -R diaginfo` remains as a manual trigger (hotkey),
  which writes an extended dump to the log.

## v0.4 — DB scraper (PostgreSQL) ✅

Goal: **distinguish "server is sick" from "DB is sick"**, since for Zabbix
the DB is the most common failure point.

- [x] Module **`src/db.rs`**: `tokio-postgres` (NoTls), `DbTarget(url)`,
  `fetch_db_stats()` with per-query timeouts.
- [x] PostgreSQL metrics:
  - `pg_stat_activity`: connections by state (active/idle/idle-in-tx/waiting).
  - Top-10 long-running queries (by `now() - query_start`) with wait_event.
  - `pg_locks` waiters (count).
  - Zabbix table sizes: `history*`, `trends*`, `events`, `event_recovery`,
    `problem` — sorted by `pg_total_relation_size`.
  - Replication lag via `pg_is_in_recovery()` +
    `pg_last_xact_replay_timestamp()`.
  - `server_version`.
- [x] CLI: `--db-url postgres://...` (or env `ZTOP_DB_URL`). If not set,
  the DB source is completely disabled.
- [x] Panel **"Database" (tab 5)**: summary with replication lag/locks, top queries
  with age color coding, ASCII bars for table sizes.
- [x] Source badge `[db ✓]` in the header.

## v0.4b — alert chord + persistent PG ✅

The main point: the whole purpose of the multi-layer architecture is **signal intersection**.
Each rule takes ≥2 sources and produces a diagnosis that cannot be obtained by
looking at a single tab.

- [x] New module **`src/diagnose.rs`**: `Severity`, `Diagnosis`,
  `diagnose(&App) -> Vec<Diagnosis>` with 6 rules:
  - `history not landing in DB` (stats `history syncer busy > 85%` **AND**
    DB queries on `history*` waiting / `connections.waiting > 0`)
  - `DB lock contention` (`locks_waiting > 0` or queries with `wait_event = Lock:*`)
  - `idle-in-transaction zombie` (idle-in-tx > 60s, blocks housekeeper)
  - `queue grows while workers idle` (`queue > 1000` **AND** all processes < 30%)
  - `<role> waiting, not running` (Δ busy(zbx) - busy(ps) ≥ 30%)
  - `replica lag` (> 30s)
- [x] **UI diagnosis strip** between tabs and body, dynamic height 0–3 lines,
  colors by severity (Critical/Warning/Info).
- [x] **Persistent PG connection**: `DbConnection` (Drop with abort), `spawn_db`
  keeps `Option<DbConnection>`, reconnects only on error. Saves ~3–5 ms per poll
  on localhost.
- [x] Unit tests for each rule (positive + healthy case).

## v0.4c — MySQL/MariaDB ✅

- [x] `mysql_async = "0.34"` (native-tls by default, option to switch to
  rustls in Cargo.toml comment).
- [x] Shared **`DbBackend` enum** in `src/db.rs`: routing by URL scheme —
  `postgres://` / `postgresql://` → PG, `mysql://` → MySQL, no scheme → PG.
- [x] `MyConnection` with **try-or-default per-query**: each query wrapped in
  `if let Ok(...)`, missing views or old MySQL versions don't break the fetch.
  Compatible with MariaDB / MySQL 5.7 / 8.0+.
- [x] MySQL queries:
  - `SELECT VERSION()`
  - `information_schema.PROCESSLIST` (grouped by COMMAND+STATE) — mapped to
    our active/idle/waiting/other; `Sleep` = idle, `Query`+`Lock` = waiting.
  - Top-10 long queries from PROCESSLIST with `state.contains("Waiting")` →
    moved to `wait_event` (rough approximation of PG events).
  - `INNODB_LOCK_WAITS` (5.7) → fallback `performance_schema.data_lock_waits` (8.0+).
  - Zabbix table sizes via `information_schema.TABLES`.
  - `SHOW REPLICA STATUS` (8.0.22+) → fallback `SHOW SLAVE STATUS`, reading
    `Seconds_Behind_Source` or `Seconds_Behind_Master`.
- [x] Field `DbStats.backend` ("postgres"/"mysql") — UI shows the correct
  label in the Database tab header.

## v0.4c.1 — MySQL parity with PostgreSQL ✅

- [x] **Precise `wait_event`** via JOIN `information_schema.PROCESSLIST` +
  `performance_schema.threads` + `performance_schema.events_waits_current`.
  If perf_schema is disabled or access is missing — try-or-default fallback to
  simple PROCESSLIST (as in v0.4c).
- [x] **idle-in-transaction** via `information_schema.innodb_trx`:
  - Counts → `connections.idle_in_transaction` (subtracted from `idle`
    to avoid double-counting — PROCESSLIST shows them as Sleep).
  - Zombies with age ≥ 60s → synthetic `LongQuery` records with
    `state = "idle in transaction"`. This allows the existing
    `rule_idle_in_tx_zombie` alert rule to fire for MySQL as well.

## v0.4d — TLS ✅ (pgpass deferred)

- [x] **TLS to PG** via `postgres-native-tls` + `native-tls`. Not feature-gated,
  just enabled — `sslmode=disable` in the URL bypasses TLS with no overhead.
  MySQL TLS worked out of the box via mysql_async.
- [x] **`--db-insecure-tls`** (env `ZTOP_DB_INSECURE_TLS`) — for self-signed
  and private CAs. A separate flag to avoid accidentally disabling verification
  in production.
- [x] SCRAM-SHA-256 works out of the box in `tokio-postgres` — no separate
  support needed (included in its default features).

## v0.4d.1 — pgpass / my.cnf ✅

- [x] New module **`src/dbcreds.rs`**:
  - `enrich_pg_url(url) -> String` / `enrich_my_url(url) -> String`
  - URL parser (scheme/user/pass/host/port/db/query), reassembly with
    percent-encoded password.
  - Reading **`~/.pgpass`** or `$PGPASSFILE`. File permission check
    (Unix: ≤ 0600). Support for `*` wildcard in any field, backslash-escape `:` and `\\`.
  - Reading **`~/.my.cnf`** sections `[client]`, `[mysql]`, `[client-server]`.
    Support for quoted values. If the my.cnf user differs from the URL user —
    password is not substituted (protection against identity confusion).
- [x] Hooked into `main.rs`: enrich URL before `DbTarget::new` based on scheme.
  If the URL already has a password — enrichment is skipped.
- [x] Unit tests: URL parsing, rebuild with percent-encoding, pgpass split with
  escape, my.cnf section parsing, pct round-trip.

## v0.5a — multi-host (MVP) ✅

- [x] Config **`~/.config/ztop/hosts.toml`** (or explicit `--config <path>`),
  TOML with a `[[host]]` array. Single-host via CLI remains as fallback
  when no config exists.
- [x] Extracted **`HostState`** from `App`. `App.hosts: Vec<HostState>` +
  `focused_host: usize`. All per-host data (procs/sys/logs/stats/db/
  history/sources/log_stream) moved into HostState.
- [x] **`HostMsg { host_idx, msg }`** — each collector wraps CollectorMsg in a
  HostMsg before sending. Event loop routes by index:
  `app.hosts.get_mut(host_msg.host_idx).apply_msg(...)`.
- [x] New tab **`0 Overview`** with a health indicator per host:
  number of healthy sources out of total, CPU/Mem/Load/Queue, active diagnosis count,
  focus marker.
- [x] **Ctrl-N / Ctrl-P** — switch focus between hosts (works from any tab).
  Current host name is shown in the header and tabs.
- [x] **Diagnoses panel — now across all hosts** with `[hostname]` prefix.
  Top-3 most-severe across the fleet.
- [x] Runtime control (`+/-/c/h/d/R`) runs on the focused host;
  toasts are also prefixed with the host name.

## v0.5b — multi-host follow-up ✅

- [x] **`zabbix_proxy:` proctitle** — the `TITLE` regex and awk filter in
  `fetch_procs` now accept both `server` and `proxy`. Same parser,
  same roles (poller/trapper/history syncer/...). Proxy-specific roles
  (data sender, heartbeat sender, vmware collector) are recognized
  automatically — the regex is not tied to a whitelist of names.
- [x] **Cursor navigation on Overview**: `↑`/`↓` (or `k`/`j`) move the cursor
  through rows, **Enter** — switches focused_host and immediately goes to
  drill-down (Tab::Processes). When entering Overview via `0`, cursor
  syncs with the current focused_host. Two markers:
    - `▸` — cursor (current selection),
    - `●` — focused (which host is shown in drill-down tabs).
- [x] **Aggregate panel** below the Overview table — fleet-level metrics:
    - Σ CPU (sum across all hosts), max load1, mem avg/max%, healthy ratio
    - Σ queue (sum of queue sizes), total diagnosis count with
      critical highlighted separately.

## v0.5c — large deployments (modal switcher) ✅

- [x] **Modal host switcher** with fuzzy search via `Ctrl-G`. 60%×60% overlay
  with a query field, "N/M match" counter, and ranked list.
- [x] **Fuzzy subsequence matcher** (`app::fuzzy_score`): score per match + bonus
  for consecutive runs + bonus for start-of-string.
  "prod" matches "zbx-prod-01" better than scattered "p…r…o…d".
- [x] **Search by two fields**: `name` and `ssh_host` — the max score is taken.
- [x] **Two cursor markers**: `▸` (current selection), `●` (focused host),
  combo `▸●`.
- [x] Unit tests for subsequence/consecutive/empty-query.

## v0.5d — remaining ideas for large deployments

- [ ] **Host grouping** by environment/region/datacenter in Overview
  (sub-headers in the table).
- [ ] **Saved layouts**: remember state (focused host, tab, paused)
  between sessions in `~/.cache/ztop/state.json`.
- [ ] **Parallel config load**: currently `DbBackend::connect` is sequential at
  startup; with 30+ hosts this is slow — needs `join_all`.
- [ ] **Highlight matched chars** in the host picker — highlight in yellow
  the letters that matched in the subsequence.

## v0.6 — recording and post-mortem ✅

Goal: **"yesterday at 03:14 there was an incident"** — analyze after the fact.

- [x] New module **`src/record.rs`** with JSONL format:
  - `HeaderLine` (version, started_at, list of HostMeta) — first line.
  - `EventLine { ts, host_idx, msg }` — one line written per `HostMsg`.
  - `RecordedMsg` — serializable mirror of `CollectorMsg` (anyhow::Error → String).
  - All state types received `#[derive(Serialize, Deserialize)]`.
- [x] **`--record path.jsonl`**: open file, write header with hosts,
  recorder.write called in event loop before apply_msg. BufWriter
  batches, Drop flushes.
- [x] **`--replay path.jsonl`**: read_header → reconstruct HostState
  list → spawn replay_task that sends HostMsg to the same mpsc channel
  with delays based on original timestamps. Speed override via
  `--replay-speed` (1.0 default, >1 = faster).
- [x] In replay mode **runtime control is disabled** (toast "disabled
  in --replay mode"), refresh/reconnect are no-ops.
- [x] **UI indicator** in the header: `● REC (N ev)` in red or `▶ REPLAY` in blue.
- [x] **Diagnoses work in replay** automatically — they are built on top of
  HostState which is reconstructed from events. You can scrub through the
  incident recording and see when each `[CRIT] history not landing in DB` lit up.

## v0.6.1 — interactive replay ✅

- [x] **`Space` — pause/resume**: replay_loop via `tokio::select!` with
  conditional branch `if !paused` on sleep; commands via an unbounded mpsc channel.
- [x] **`n` — step**: advances by one event (useful while paused).
- [x] **`>` / `.` — seek forward 60s**: cursor via `partition_point`
  by timestamp; anchors are recalculated.
- [x] **`<` / `,` — seek backward 60s**: cursor back + **synthetic
  `CollectorMsg::Reset`** for all hosts, otherwise UI would show data
  from the "future". State rebuilds naturally from incoming events.
- [x] In-memory load of the full recording (`load_recording`) — enables seek in
  both directions. Lazy-streaming from v0.6 only supported forward.
- [x] **UI progress**: `▶ REPLAY 45.2%` or `⏸ REPLAY 45.2% [PAUSED]`,
  atomic counter 0..=1000 millipercent updated by replay_loop.

## v0.6.2 — post-mortem pro (deferred)

- [ ] **Compare mode**: two time windows side by side, diff by roles.
- [ ] **Screen export** to `ztop-YYYYMMDD-HHMMSS.txt` via hotkey.
- [ ] **Compaction**: `ztop --compact src.jsonl dst.jsonl` removes
  redundant LogStreamLine entries between ps snapshots.
- [ ] **Seek-to-diagnose**: jump to the moment when a specific
  Diagnosis first fired.
- [ ] **Bookmarks**: marks on interesting moments in the recording (`b` sets, `B` lists).

## v0.7 — diagnostic probes ✅

Goal: **separate "Zabbix itself is sick" from "DB/network/upstream is sick"**. Probes
run **from the machine running ztop**, allowing you to immediately see whether latency
is on my specific laptop/network or the server/DB is genuinely failing.

- [x] New module **`src/probes.rs`**: `ProbeConfig`, `ProbeState`,
  `ProbeMsg`, `spawn_probes`. Each probe is a separate tokio task with
  its own interval and timeout.
- [x] **TCP probe**: `TcpStream::connect(host:port)` with timing.
- [x] **DNS probe**: `tokio::net::lookup_host` resolve latency.
- [x] **PG probe**: open connection (`tokio_postgres::connect`),
  `SELECT 1`, close. Via `dbcreds::enrich_pg_url`, so passwords
  from ~/.pgpass work.
- [x] **Config**: `[[probe]]` blocks in the same `hosts.toml` with a `kind` field.
- [x] **New tab `6 Probes`**: table with name/kind/target/latency/status/
  success%/last_error. Color-coded latency (green <100ms,
  yellow <1s, red >1s) and success% (>99% green, >90% yellow, otherwise red).
  Passwords in URLs are masked as `****`.
- [x] **Opt-in**: without `[[probe]]` blocks the tab shows a hint with an example.

Deferred to v0.7.1:
- [ ] **TLS handshake probe** via `tokio-native-tls`.
- [ ] **MySQL probe** via `mysql_async`.
- [ ] **HTTP probe**: GET with status code and timing (requires `reqwest`).
- [ ] **Latency histogram** over N seconds (percentiles).
- [ ] **Cross-probe diagnose rules**: "PG ping OK but DB source on the Zabbix host fails —
  the problem is either in the network between hosts or in user permissions".

## v0.8+ — polish

- [ ] Help overlay via `?`, hotkey cheatsheet.
- [ ] Config file for overriding hotkeys, colors, thresholds.
- [ ] Theme: dark/light/colorblind presets.
- [ ] Localization (RU/EN), currently English in code.
- [ ] Distribution: prebuilt binaries in release, Homebrew tap, deb/rpm,
  Docker image for CI probes.

---

## Testbed / dev infrastructure ✅

- [x] **`docker/`** — full Zabbix stack for debugging ztop:
  - PG + zabbix-server (custom image with sshd) + agent + web frontend
  - SSH keys generated by `setup.sh`, sudoers for runtime control
  - Ready-made `hosts.toml` — `ztop --config docker/hosts.toml` and go
- [x] **6 stress scenarios** in `docker/stress/`:
  - `stress_trapper.sh` — bulk values via `zabbix_sender`
  - `stress_api.sh` — curl loop on JSON-RPC API
  - `stress_db_slow.sh` — parallel `SELECT pg_sleep(N)`
  - `stress_db_zombie_tx.sh` — idle-in-transaction simulator
  - `stress_log_spam.sh` — ramp log_level to test log streaming
  - `stress_history_flood.sh` — **combo to trigger `[CRIT] history not
    landing in DB`**, the main diagnose rule
- [x] Each stress script documents what should light up in ztop.
  This turns manual verification into "run and confirm".

Deferred:
- [ ] **CI smoke test**: GitHub Actions brings up the testbed, runs stress
  scenarios, reads progress via record/replay, verifies expected
  diagnoses fired. This turns stress scenarios into e2e tests.

## Cross-cutting tasks (not tied to a version)

- [ ] CI on GitHub Actions: `cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo test`, build artifacts for Linux/macOS.
- [ ] Parser benchmarks (`criterion`) — so 1000-fork hosts don't lag.
- [ ] Man page (via `clap_mangen`).
- [ ] Asciinema demo in README.
- [ ] Tracing/logs from ztop itself to `~/.cache/ztop/ztop.log` for diagnosing its own hangs.

---

## What we intentionally do NOT do

- **No Zabbix HTTP API (`api_jsonrpc.php`) integration.** By design —
  it depends on PHP/Apache/DB, which are the most fragile parts. Internal
  metrics come via `zabbix.stats` on the trapper port (see v0.3) — the same
  binary protocol used by Zabbix agents, without the PHP stack.
- **No agent for Zabbix itself.** ztop is an external observer, not a data source.
- **No web UI/HTTP endpoint.** That would compete with Zabbix's own frontend,
  which is not the project's goal.
- **No custom SSH stack from scratch.** russh — yes, libssh2 — yes, custom — no.

---

## "When we get to it" decisions

Not priorities, but checkpoints — what will trigger picking up a task.

| Trigger                                               | Task               |
|-------------------------------------------------------|--------------------|
| If ztop itself starts lagging on large installations  | v0.2 long SSH      |
| If something critical slips between poll ticks in logs | v0.2 tail -F       |
| If there's an incident where busy% ≠ CPU% significantly | v0.3 zabbix.stats |
| If we catch an incident "DB is down, server is alive" | v0.4 DB scraper    |
| If a second Zabbix server comes under management      | v0.5 multi-host    |
| If we have to write a post-mortem without full telemetry | v0.6 record/replay |
