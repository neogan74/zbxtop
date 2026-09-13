# ztop

> [English documentation](../README.md)

Консольный (TUI) монитор внутреннего состояния **zabbix_server** по SSH.
Цель — наблюдать за процессами и логами Zabbix-сервера тогда, когда веб-UI/API
тормозят или недоступны: данные собираются напрямую из ОС (`ps`, `/proc`, log
tail) и через runtime-control команды самого `zabbix_server`.

## Что показывает

- **Шапка** — общие метрики хоста (load, mem, swap, uptime) и **бейджи по
  каждому источнику данных**: `[ps ✓] 1s ago, 23ms` для ps/sys/stats,
  `[log ●] streaming` для стрима. Цвет показывает здоровье: зелёный — свежо,
  жёлтый — STALE (>10s без обновления), красный — не отвечает. Видно сразу,
  какой коллектор тупит.
- **Processes** — таблица агрегатов по ролям форков `zabbix_server` (poller,
  trapper, history syncer, preprocessing worker, lld manager, escalator, ...):
  количество, суммарный CPU% (из ps), RSS, **BusyPs%** (% форков не-idle из
  proctitle), **BusyZbx%** (busy.avg из `zabbix.stats`), **Δ** — расхождение.
  Большой положительный Δ красным = сервер считает себя занятым, а ps этого
  не видит → форки ждут на lock/DB/IO.
- **Internals** — данные напрямую от `zabbix_server` через trapper-порт без
  PHP/Apache/БД: version, uptime, очередь, vps, busy% per process type c
  avg/max/min, утилизация write/read/value кэшей. Этот таб работает даже
  когда веб-UI лежит.
- **Database** (PostgreSQL) — третий слой: connections by state (active/idle/
  idle-in-tx/waiting), top long-running queries c wait events, locks waiting,
  размеры zabbix-таблиц (history/trends/events с ASCII-баром), репликационный
  лаг. Видно отдельно от Zabbix-сервера: PG может лагать, пока сервер ещё
  считает себя здоровым.
- **Diagnoses** (полоса над body, видна на любом табе) — **алерт-аккорд**:
  правила, которые опираются на ≥2 источника одновременно. Примеры:
  «history syncer busy 95% + waiting INSERT в `history*` → история не
  успевает в БД», «Δ busy(zbx) - busy(ps) = +50% → процесс ждёт, а не
  считает», «idle-in-tx > 600s → housekeeper заблокирован». Цветовая
  градация Critical / Warning / Info, показывается до 3 диагнозов
  одновременно, секция исчезает если всё ок.
- **Graphs** — ASCII-спарклайны последних ~8 минут: суммарный CPU форков,
  используемая память, loadavg(1m).
- **Logs** — **real-time streaming** `zabbix_server.log` через `tail -F` по
  одному long-lived ssh-каналу. Строки появляются мгновенно, не пакетами.
  Подсветка уровня (error/warning/info) и live-фильтр. Бейдж в шапке
  показывает статус стрима (`●` streaming / `○` reconnecting ×N).
- **Runtime control** — горячие клавиши и модальное меню для команд
  `zabbix_server -R`: log_level_increase/decrease, config_cache_reload,
  snmp_cache_reload, housekeeper_execute, diaginfo.

API Zabbix **не используется принципиально** — инструмент должен работать,
когда сама база/UI/API тормозят.

## Локальный testbed (docker-compose)

Не хочешь рисковать прод-Zabbix-ом? Подними локально весь стек через
docker-compose — все четыре слоя (OS / Zabbix / PG / probes) работают
end-to-end. Со stress-сценариями, которые умеют поджигать конкретные
правила diagnose:

```
cd docker && ./setup.sh && docker compose up -d --build
cargo run --release -- --config docker/hosts.toml
```

Подробнее: [docker/README.md](docker/README.md).

## Сборка

```
cargo build --release
```

Готовый бинарь — `target/release/ztop` (~3–5 МБ, статически линкованный
большинством зависимостей).

## Запуск

**Single-host (CLI):**

```
ztop --host zbx-prod-01 \
     --log  /var/log/zabbix/zabbix_server.log \
     --sudo                 # опционально, см. ниже
     --stats-port 10051     # default
     # --no-stats           # отключить trapper-источник, если недоступен
```

Все флаги дублируются переменными окружения: `ZTOP_HOST`, `ZTOP_LOG`,
`ZTOP_SUDO`, `ZTOP_STATS_HOST`, `ZTOP_STATS_PORT`, `ZTOP_NO_STATS`,
`ZTOP_CONFIG`, `ZTOP_DB_URL`, `ZTOP_DB_INSECURE_TLS`.

**Multi-host через конфиг (v0.5a):**

Создай `~/.config/ztop/hosts.toml` (или `--config <path>`):

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
no_stats = true     # trapper закрыт fw

[[host]]
name = "dev"
ssh = "zbx-dev"
log = "/srv/zabbix/log/server.log"
db_url = "mysql://ztop_ro:secret@db-dev/zabbix"
```

И просто `ztop` — конфиг подхватится из дефолтного пути. В UI появится
таб **`0 Overview`** с health-светофором по всем хостам, под таблицей —
**Fleet aggregate** (суммарный CPU/queue, max load, mem avg/max, общий
счётчик диагнозов с выделением critical).

Переключение между хостами:
- **Ctrl-N / Ctrl-P** — на любом табе, последовательный перебор.
- На Overview — `↑`/`↓` (или `j`/`k`) двигают **курсор** (`▸`), **Enter**
  ставит focused host (`●`) и сразу уходит на `1 Processes`.
- **Ctrl-G** (v0.5c) — модал host picker с **fuzzy search** по имени и
  ssh-таргету. Набирай несколько букв — список сужается. Полезно на 20+
  хостах, где Ctrl-N/P перебирать долго. Например, на `prod` найдутся
  все production-хосты с consecutive-бонусом за плотное совпадение.

Диагнозы накапливаются across the fleet с префиксом `[hostname]`, top-3
most-severe видны над body независимо от текущего таба.

Поддерживается **zabbix_proxy** (v0.5b): прокси-специфичные роли
(`data sender`, `heartbeat sender`, `vmware collector`) распознаются
автоматически. В `hosts.toml` всё одинаково для server и proxy.

## Синтетические пробы (v0.7)

ztop умеет дёргать **дешёвые пробы с машины, где он запущен**: TCP-connect,
DNS-resolve, PG `SELECT 1`. Цель — сразу видеть, лагает ли это **сеть с
моего ноутбука** или реально болеет сервер/БД. Это четвёртый «слой
живучести» — он самый ближний к человеку, не требует доступа к
удалённому хосту.

Добавь блоки `[[probe]]` в тот же `hosts.toml`:

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

В таб `6 Probes` появится таблица: name / kind / target / latency / status /
success% / last error. Цветовая шкала latency: зелёный <100ms, жёлтый
<1s, красный >1s. Пароли в URL маскируются `****` для безопасного
отображения.

Пробы — **opt-in**, без блоков `[[probe]]` ничего не пингуем (нельзя
случайно DDoSнуть прод). PG-пробы используют ту же логику чтения
паролей из `.pgpass`, что и основной DB-источник.

## Запись и воспроизведение (v0.6)

Для постмортема инцидентов — пишем всю поступающую телеметрию в файл,
потом проигрываем как live-сессию.

**Записать инцидент:**

```
ztop --record incident-2026-05-13.jsonl
```

В шапке появится `● REC (N ev)` — счётчик записанных событий. Файл —
JSON Lines, header первой строкой со списком хостов, дальше каждое
сообщение коллектора с timestamp-ом.

**Проиграть запись:**

```
ztop --replay incident-2026-05-13.jsonl
ztop --replay incident.jsonl --replay-speed 10    # в 10 раз быстрее
```

Хосты восстанавливаются из header-а — `--host`/`--config` не нужны.
В шапке `▶ REPLAY 45.2%` (прогресс). Runtime control (`R`, `+/-`, `c`,
`h`, `d`) отключен, SSH-коннекты не открываются. Всё остальное работает:
переключение табов, фокус хостов, фильтры лога, **алерт-аккорд
диагнозов** — он построен поверх state и срабатывает точно так же, как
был в записи. Можно перематывать инцидент и видеть, когда какой диагноз
загорелся.

**Управление replay (v0.6.1):**

| Клавиша     | Действие |
|-------------|----------|
| `Space`     | Pause / resume |
| `n`         | Step — один event вперёд (в paused) |
| `>` или `.` | Seek 60 сек вперёд |
| `<` или `,` | Seek 60 сек назад (сбрасывает state, перестраивается по новым событиям) |

При прыжке назад state хостов **сбрасывается** через синтетический
`CollectorMsg::Reset` — иначе на экране были бы данные «из будущего»
относительно нового положения курсора. По мере воспроизведения дальше
state перестраивается естественно из событий.

**Формат файла** (JSON Lines):

```json
{"kind":"header","version":"0.6","started_at":"2026-05-13T03:00:00Z","hosts":[...]}
{"kind":"event","ts":"2026-05-13T03:14:23.123Z","host_idx":0,"msg":{"type":"Procs","result":{"Ok":[...]},"took_ms":23}}
{"kind":"event","ts":"2026-05-13T03:14:23.450Z","host_idx":0,"msg":{"type":"LogStreamLine","raw":"...","level":"Error"}}
```

Объём: при 3-host setup и умеренной активности — ~10-50 KB/sec, за
час инцидента ~36-180 MB. Compaction отложен в v0.6.1.

## Настройка zabbix.stats (TCP 10051)

`zabbix_server.conf` должен разрешить IP, с которого ztop подключается:

```
StatsAllowedIP=10.0.0.1,192.168.0.0/24
# или просто 127.0.0.1 если ztop запущен на той же машине
```

Это **бинарный Zabbix-протокол** на trapper-порту, не HTTP API. Никаких
токенов — IP-ACL и всё. Работает, пока жив сам процесс `zabbix_server`,
даже если PHP/Apache/БД упали.

Проверить руками: `echo '{"request":"zabbix.stats"}' | nc <host> 10051`
(ответ будет с 13-байтовым ZBXD-header-ом, дальше JSON).

## DB-скрейпер (PostgreSQL + MySQL/MariaDB)

ztop ходит в БД напрямую и опрашивает 6 запросов: коннекты, top long
queries, locks waiting, размеры zabbix-таблиц, replication lag, version.

Backend выбирается по схеме URL:
- `postgres://...` или `postgresql://...` → PostgreSQL (`tokio-postgres`)
- `mysql://...` → MySQL/MariaDB (`mysql_async`)
- без схемы — default PG (libpq key=value тоже сюда)

**Read-only роль обязательна.** На стороне PG создать так:

```sql
CREATE ROLE ztop_ro WITH LOGIN PASSWORD '<...>';
GRANT pg_read_all_stats TO ztop_ro;       -- pg_stat_activity, pg_locks и т.д.
GRANT CONNECT ON DATABASE zabbix TO ztop_ro;
GRANT USAGE  ON SCHEMA public TO ztop_ro;
-- Только для размеров таблиц (read метаданных):
GRANT SELECT ON pg_catalog.pg_class      TO ztop_ro;
GRANT SELECT ON pg_catalog.pg_namespace  TO ztop_ro;
```

Запуск:

```
ztop --host zbx-prod-01 \
     --db-url 'postgres://ztop_ro:<pass>@db-host:5432/zabbix?sslmode=disable'
```

Пароль из URL виден в `ps`/`/proc`. На проде лучше через env:

```
export ZTOP_DB_URL='postgres://ztop_ro:<pass>@db-host:5432/zabbix'
ztop --host zbx-prod-01
```

Если PG недоступен (firewall, нет креды) — `--db-url` просто не передаётся,
DB-таб покажет hint, остальные источники работают как раньше.

**MySQL/MariaDB read-only setup:**

```sql
CREATE USER 'ztop_ro'@'%' IDENTIFIED BY '<...>';
GRANT SELECT, PROCESS, REPLICATION CLIENT, REPLICATION SLAVE ADMIN ON *.* TO 'ztop_ro'@'%';
-- PROCESS нужен для information_schema.PROCESSLIST и .innodb_trx
-- REPLICATION CLIENT — для SHOW REPLICA STATUS
-- SELECT на information_schema и performance_schema даётся неявно
FLUSH PRIVILEGES;
```

Для `wait_event` нужен включённый performance_schema (обычно по умолчанию
на MySQL 5.6+ и MariaDB 10.5+). Если выключен или нет SELECT-прав — ztop
автоматически переключится на упрощённую PROCESSLIST без `wait_event`.

Запуск с MySQL:

```
ztop --host zbx-prod-01 \
     --db-url 'mysql://ztop_ro:<pass>@db-host:3306/zabbix?ssl-mode=REQUIRED'
```

## TLS до БД (v0.4d)

**PostgreSQL.** TLS-коннектор всегда подключён через `postgres-native-tls`,
действие управляется параметром URL `sslmode=`:
- `sslmode=disable` — TCP без TLS (по дефолту в URL без явного указания).
- `sslmode=prefer` — TLS если сервер поддерживает, иначе plain (libpq default).
- `sslmode=require` — TLS обязательно, верификация CA не требуется.
- `sslmode=verify-ca`/`verify-full` — TLS + полная верификация.

Для self-signed сертификатов или приватных CA на внутреннем периметре
есть флаг `--db-insecure-tls` (env `ZTOP_DB_INSECURE_TLS=true`): принимает
любой cert без верификации. **Не использовать на проде через WAN.**

**MySQL.** TLS управляется параметрами URL через mysql_async:
- `?ssl-mode=DISABLED` — без TLS
- `?ssl-mode=REQUIRED` — TLS обязательно, без верификации CA
- `?ssl-mode=VERIFY_CA`/`VERIFY_IDENTITY` — полная верификация

## Пароли из ~/.pgpass и ~/.my.cnf (v0.4d.1)

Если в `--db-url` пароль не указан (`postgres://user@host/db`), ztop
читает пароль из стандартных libpq-/mysql-совместимых файлов.

**PostgreSQL — `~/.pgpass`** (или `$PGPASSFILE`):

```
# host:port:database:user:password
db-host:5432:zabbix:ztop_ro:s3cret
*:5432:*:ztop_ro:fallback-password
```

Правила libpq: файл должен иметь права `0600` (или `0400`), иначе игнор.
Звёздочка `*` — wildcard. Backslash-escape для `:` и `\` в полях.

**MySQL — `~/.my.cnf`** секция `[client]`:

```ini
[client]
user = ztop_ro
password = "s3:cret"
host = db-host
port = 3306
```

Если в `[client]` указан `user` и он не совпадает с user в URL — пароль
не подставляется (защита от случайного использования чужой identity).

После обогащения URL передаётся в драйвер обычным путём. Если пароль
из файла прочитать не удалось (нет файла, неверные права, нет матча) —
URL отправляется как есть, и сервер вернёт auth error.

Что **не делается** в v0.4c (отложено в v0.4d): TLS до PG/MySQL,
`.pgpass`/`my.cnf` чтение паролей, persistent connection — уже сделано в
v0.4b. Точный wait_event у MySQL через `performance_schema.events_waits_current` —
v0.4c.1. idle-in-transaction для MySQL через `innodb_trx` — там же.

## Настройка SSH

Прототип вызывает системный `ssh` — значит, всё, что у вас уже работает в
`~/.ssh/config`, работает и здесь: alias-ы хостов, ProxyJump, ключи, агент.

Рекомендуется добавить мультиплексирование в `~/.ssh/config`, чтобы повторные
команды летели через один TCP-сокет (snappy refresh):

```sshconfig
Host zbx-prod-01
    HostName zbx-prod-01.internal
    User      ztop
    ControlMaster auto
    ControlPath   /tmp/ztop-%r@%h:%p
    ControlPersist 60s
```

ztop сам прокидывает эти опции, но явное указание в config удобно для отладки.

## Права на runtime control

`zabbix_server -R` работает от любого пользователя, который может писать в
runtime-сокет сервера — обычно это `zabbix` или `root`. Варианты:

1. **Логиниться по SSH под пользователем zabbix** — простейший вариант,
   `--sudo` не нужен.
2. **Логиниться под обычным пользователем + `--sudo`** — добавить sudoers:
   ```
   ztopuser ALL=(root) NOPASSWD: /usr/sbin/zabbix_server -R *
   ```
   ztop вызывает `sudo -n` (без пароля), так что NOPASSWD обязателен.

Для просмотра логов и `ps`/`/proc` обычно sudo не требуется.

## Структура

```
src/
  main.rs          — CLI, terminal setup, channel-based event loop
  app.rs           — состояние, кольцевые буферы истории, apply_msg
  source.rs        — per-collector tokio tasks с backoff + общий Notify
  ssh.rs           — обёртка вокруг system ssh (tokio::process)
  collectors.rs    — парсеры ps/loadavg/meminfo/uptime/logs + runtime control
  zbxstats.rs      — бинарный Zabbix-протокол: framing + fetch_stats (v0.3)
  db.rs            — PG + MySQL backends: DbBackend enum, TLS (v0.4/v0.4b-d)
  dbcreds.rs       — pgpass / my.cnf чтение паролей (v0.4d.1)
  diagnose.rs      — cross-source правила: пересечения сигналов (v0.4b)
  hosts.rs         — TOML-конфиг multi-host (v0.5a)
  record.rs        — запись и replay телеметрии в JSONL (v0.6/v0.6.1)
  probes.rs        — синтетические пробы TCP/DNS/PG (v0.7)
  ui/              — ratatui-рендеринг: chrome (header/tabs/footer),
                     по модулю на таб, модалы, общие хелперы
```

**Архитектура опроса (v0.2a+b).** Каждый коллектор живёт в своей
`tokio::spawn`-задаче, отправляет результаты в общий `mpsc`-канал.
Event-loop обновляет state по приходу сообщений + рисует экран ещё и по
UI-тикеру (для декая toast и пересчёта STALE-таймеров).

- **Procs/Sys (periodic)** — каждые 2 сек дёргают `ssh ... 'cmd'`, на ошибках
  делают экспоненциальный backoff (cap 30s).
- **Logs (streaming, v0.2b)** — один long-lived ssh-процесс с `tail -n N -F`,
  построчно отдаёт строки в канал. Reconnect с backoff при EOF/ошибке.
- **Force-refresh `r`** — общий `Notify` будит все periodic-задачи сразу.
- **Force-reconnect `L`** — отдельный Notify для перезапуска стрима логов.

Unit-тесты парсеров: `cargo test`.

## Что в планах после MVP

1. **Стрим логов через одну долгую SSH-сессию** (`russh` + `tail -F`), чтобы не
   опрашивать поллингом.
2. **Diaginfo → структурированные блоки**: запустить `zabbix_server -R
   diaginfo`, дождаться записи в log, распарсить секции historycache /
   valuecache / preprocessing и показать их в отдельной панели.
3. **Multi-host** — отдельная вкладка с несколькими Zabbix-серверами.
4. **Очередь** — `zabbix_get`/SQL/`zabbix_server -R diaginfo` для размера
   очереди по prefer-зонам, отдельный график.
5. **Кэширование результатов на диск** для post-mortem (CSV/JSONL `--record`).
6. **Прокси-форки** — расширить парсер на `zabbix_proxy:` (тот же proctitle).
7. **TLS-status/DB latency** — простые синтетические пробы (`time psql -c ...`,
   `nc -z`) для гипотезы «база тупит».
