# ztop testbed (docker-compose)

Local Zabbix stack for testing and verifying ztop in a safe environment.
Covers all four layers of the ztop architecture:

| Layer                | Source in testbed                          |
|----------------------|--------------------------------------------|
| OS (SSH)             | `zabbix-server` container + sshd          |
| Zabbix-server (TCP)  | trapper port 10051                         |
| PostgreSQL           | `postgres` container on 55432             |
| Probes (local)       | probes from `hosts.toml`                  |

## Stack composition

- **postgres:15** — Zabbix database (credentials `zabbix:zabbix`)
- **zabbix-server-pgsql** + sshd (our custom image) — backend server
- **zabbix-agent2** — for self-monitoring + convenient `zabbix_sender`
  source in stress scenarios
- **zabbix-web-nginx-pgsql** — frontend + JSON-RPC API (mock target for
  stress)

## Starting up

```bash
cd docker

# 1. Generate SSH key and get instructions
./setup.sh

# 2. Add the block to ~/.ssh/config (from setup.sh output)

# 3. Start the stack
docker compose up -d --build

# 4. Wait ~30 seconds for DB and web initialisation
docker compose ps
docker compose logs zabbix-server | tail -20

# 5. Run ztop
cd ..
cargo run --release -- --config docker/hosts.toml
```

In the UI all sources should light up: `[ps ✓]` `[sys ✓]` `[log ●]`
`[stats ✓]` `[db ✓]`, plus probes in tab `6 Probes` — all OK.

Web UI: http://127.0.0.1:8088 (login `Admin` / password `zabbix`).

## Stress scenarios

Each scenario targets a specific rule in diagnose.rs or a specific panel in
ztop. After launching **keep ztop open in another window** and observe.

### `stress_trapper.sh [VPB] [DUR]`

Mass value submission to trapper. Default: 500 vals/sec, 60 sec.

**What will light up in ztop:**

- Tab **4 Internals**: busy% on trapper processes rises
- Tab **2 Graphs**: CPU sum spike
- vps increases in the Internals header
- Possibly `rule_busy_cpu_delta` if busy(stats) >> busy(ps)

### `stress_api.sh [DUR] [CONC]`

JSON-RPC API spam. Default: 60 sec, 5 parallel threads.

**What will light up:**

- Tab **5 Database**: `connections.active` grows
- top_queries fills with SELECTs from the PHP frontend
- Probe "Web frontend" (TCP to 8088) may slow down — visible in tab `6 Probes`

### `stress_db_slow.sh [DUR] [CONC]`

Parallel `SELECT pg_sleep(N)`. Default: 60 sec, 3 threads.

**What will light up:**

- Tab **5 Database** → Top long-running queries: pg_sleep with growing age visible
- Color scale for latency in the row: yellow >10s, red >60s
- `connections.active` jumps

### `stress_db_zombie_tx.sh [DUR]`

Opens a PG transaction and holds it idle. Default: 300 sec.

**What will light up:**

- After ~60 sec: **`[WARN] idle-in-transaction zombie`** in the diagnosis strip
- After 600 sec: severity → **Critical**
- In tab `5 Database`: `idle-in-tx` counter in the summary

### `stress_log_spam.sh {up|down}`

Raises the zabbix_server log_level to flood the log with DEBUG lines.

**What will light up:**

- Tab **3 Logs**: real-time stream, hundreds of lines/sec visible
- Badge `[log ●]` stays `streaming, last 0s ago` (stream is alive)
- Good moment to try the live filter `/` by a specific worker

Don't forget `./stress_log_spam.sh down` to restore the level afterward.

### `stress_history_flood.sh [TRAPPER_DUR]`

**Combo scenario:** zombie-tx + trapper-flood simultaneously. Default:
trapper 90 sec, zombie 300 sec.

**Goal**: trigger the most important diagnostic rule —
**`[CRIT] history not landing in DB`** (stats + db cross-source).
This is the **main test of ztop's value**: it shows that cross-source
rules actually fire on realistic workloads.

## Stopping and cleanup

```bash
docker compose down              # stop, PG volume remains
docker compose down -v           # stop + delete volume (all data lost)
```

## Troubleshooting

**SSH login failed**: check that `~/.ssh/config` contains the block from
`setup.sh` and that the path to `IdentityFile` is absolute.

```bash
# Test from CLI
ssh ztop-testbed -v
```

**Zabbix web shows "Cannot connect to server"**: zabbix-server hasn't
opened the trapper port yet. Wait 30-60 sec, or:

```bash
docker compose logs -f zabbix-server | grep "server #0 started"
```

**Probe "Web frontend" is red**: zabbix-web takes longer to start than
zabbix-server. It should go green within a minute.

**zabbix_sender returns "not supported"**: items haven't been created in the
DB yet. You can create them via the web UI (Configuration → Hosts → create
host "ztop-test" with trapper items), but for stress testing this is usually
unnecessary — even rejected values load the trapper processes.

**`stress_db_zombie_tx.sh` doesn't release the transaction**: Ctrl-C the
script itself, or forcibly:

```bash
docker compose exec postgres psql -U zabbix -d zabbix -c \
    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
     WHERE state = 'idle in transaction';"
```

## Structure

```text
docker/
├── README.md                       — this file
├── docker-compose.yml              — stack
├── Dockerfile.zabbix-server        — zabbix + sshd
├── entrypoint.sh                   — sshd + zabbix
├── setup.sh                        — ssh keygen + instructions
├── hosts.toml                      — ztop config for testbed
├── ssh-keys/.gitignore             — private key not in git
└── stress/
    ├── stress_trapper.sh
    ├── stress_api.sh
    ├── stress_db_slow.sh
    ├── stress_db_zombie_tx.sh
    ├── stress_log_spam.sh
    └── stress_history_flood.sh
```
