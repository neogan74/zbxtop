# ztop testbed (docker-compose)

Локальный Zabbix-стек для отладки и проверки ztop в безопасных условиях.
Покрывает все четыре слоя архитектуры ztop:

| Слой                 | Источник в testbed                        |
|----------------------|-------------------------------------------|
| OS (SSH)             | `zabbix-server` контейнер + sshd          |
| Zabbix-server (TCP)  | trapper-порт 10051                        |
| PostgreSQL           | `postgres` контейнер на 55432             |
| Probes (local)       | пробы из `hosts.toml`                     |

## Состав стэка

- **postgres:15** — БД Zabbix (creds `zabbix:zabbix`)
- **zabbix-server-pgsql** + sshd (наш custom image) — backend-сервер
- **zabbix-agent2** — для self-monitoring + используется как удобный
  source `zabbix_sender` в stress-сценариях
- **zabbix-web-nginx-pgsql** — фронтенд + JSON-RPC API (mock-цель для
  stress)

## Запуск

```bash
cd docker

# 1. Сгенерировать SSH key и получить инструкции
./setup.sh

# 2. Вставить блок в ~/.ssh/config (из вывода setup.sh)

# 3. Поднять стэк
docker compose up -d --build

# 4. Подождать ~30 секунд для инициализации БД и веба
docker compose ps
docker compose logs zabbix-server | tail -20

# 5. Запустить ztop
cd ..
cargo run --release -- --config docker/hosts.toml
```

В UI должны зажечься все источники: `[ps ✓]` `[sys ✓]` `[log ●]`
`[stats ✓]` `[db ✓]`, плюс пробы в табе `6 Probes` — все OK.

Web UI: http://127.0.0.1:8088 (логин `Admin` / пароль `zabbix`).

## Stress-сценарии

Каждый сценарий нацелен на конкретное правило в diagnose.rs или
конкретную панель в ztop. После запуска **держи ztop открытым в другом
окне** и наблюдай.

### `stress_trapper.sh [VPB] [DUR]`

Массовая отправка values в trapper. Default: 500 vals/sec, 60 сек.

**Что зажжётся в ztop:**
- Таб **4 Internals**: busy% у trapper-процессов растёт
- Таб **2 Graphs**: CPU sum spike
- vps растёт в шапке Internals
- Возможно `rule_busy_cpu_delta` если busy(stats) >> busy(ps)

### `stress_api.sh [DUR] [CONC]`

Спам по JSON-RPC API. Default: 60 сек, 5 параллельных потоков.

**Что зажжётся:**
- Таб **5 Database**: `connections.active` растёт
- top_queries наполнится SELECT-ами от PHP-фронта
- Probe «Web frontend» (TCP к 8088) может тормозить — видно в табе `6 Probes`

### `stress_db_slow.sh [DUR] [CONC]`

Параллельные `SELECT pg_sleep(N)`. Default: 60 сек, 3 потока.

**Что зажжётся:**
- Таб **5 Database** → Top long-running queries: видны pg_sleep с age растущим
- Цветовая шкала latency в строке: жёлтая >10s, красная >60s
- `connections.active` подскакивает

### `stress_db_zombie_tx.sh [DUR]`

Открывает PG-транзакцию и держит её idle. Default: 300 сек.

**Что зажжётся:**
- Через ~60 сек: **`[WARN] idle-in-transaction zombie`** в полосе диагнозов
- Через 600 сек: severity → **Critical**
- В табе `5 Database`: `idle-in-tx` counter в summary

### `stress_log_spam.sh {up|down}`

Поднимает log_level zabbix_server, чтобы залить лог DEBUG-строками.

**Что зажжётся:**
- Таб **3 Logs**: real-time stream, видны сотни строк/сек
- Бейдж `[log ●]` остаётся `streaming, last 0s ago` (стрим живой)
- Хороший момент попробовать live-фильтр `/` по конкретному worker

Не забудь `./stress_log_spam.sh down` чтобы вернуть уровень обратно.

### `stress_history_flood.sh [TRAPPER_DUR]`

**Combo-сценарий:** zombie-tx + trapper-flood одновременно. Default:
trapper 90 сек, zombie 300 сек.

**Цель**: поджечь самое важное правило диагностики —
**`[CRIT] history not landing in DB`** (stats + db cross-source).
Это **главный тест ценности ztop**: показывает, что cross-source
правила реально срабатывают на правдоподобных нагрузках.

## Останов и cleanup

```bash
docker compose down              # стопнуть, том PG остаётся
docker compose down -v           # стопнуть + удалить том (все данные потеряются)
```

## Troubleshooting

**SSH login failed**: проверь что `~/.ssh/config` содержит блок из
`setup.sh` и что путь к `IdentityFile` абсолютный.

```bash
# Тест из CLI
ssh ztop-testbed -v
```

**Zabbix web показывает «Cannot connect to server»**: zabbix-server ещё
не открыл trapper-порт. Подожди 30-60 сек, или:

```bash
docker compose logs -f zabbix-server | grep "server #0 started"
```

**Probe «Web frontend» красная**: zabbix-web запускается дольше, чем
zabbix-server. Должна позеленеть через минуту.

**zabbix_sender отдаёт «not supported»**: items в БД ещё не созданы.
Можно их создать через web UI (Configuration → Hosts → создать host
«ztop-test» с trapper-итемами), но обычно для stress это не нужно —
даже rejected values нагружают trapper-процессы.

**`stress_db_zombie_tx.sh` не отпускает транзакцию**: Ctrl-C на самом
script, или принудительно:

```bash
docker compose exec postgres psql -U zabbix -d zabbix -c \
    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
     WHERE state = 'idle in transaction';"
```

## Структура

```
docker/
├── README.md                       — этот файл
├── docker-compose.yml              — стэк
├── Dockerfile.zabbix-server        — zabbix + sshd
├── entrypoint.sh                   — sshd + zabbix
├── setup.sh                        — ssh keygen + instructions
├── hosts.toml                      — ztop-config для testbed
├── ssh-keys/.gitignore             — приватный ключ не в git
└── stress/
    ├── stress_trapper.sh
    ├── stress_api.sh
    ├── stress_db_slow.sh
    ├── stress_db_zombie_tx.sh
    ├── stress_log_spam.sh
    └── stress_history_flood.sh
```
