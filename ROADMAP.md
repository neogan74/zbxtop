# ztop roadmap

Принцип сортировки — **«работает на больной системе»**. Каждая волна добавляет
ценность именно когда Zabbix-сервер в плохом состоянии: тормозит, льёт логи,
не отвечает UI. Чисто косметика, табы и темы — в самом низу.

## Архитектурная идея: слоёный доступ к данным

ztop собирает данные с нескольких источников разной «живучести». Чем больше
источников подключено, тем точнее ztop скажет, **что именно сломалось**.
Каждый следующий слой падает в инциденте раньше предыдущего:

| Источник                       | Жив пока...                  | Реализация |
|--------------------------------|------------------------------|------------|
| OS (SSH → `ps`, `/proc`, log)  | жива сама машина             | v0.1 (есть) |
| `zabbix.stats` TCP:10051       | жив процесс `zabbix_server`  | v0.3 |
| DB direct (`pg_stat_*`, MySQL) | жива БД (даже если сервер мёртв) | v0.4 |
| HTTP API                       | живы PHP + Apache + DB       | **осознанно не делаем** |

В UI один экран — несколько панелей с пометкой источника: пользователь
сразу видит, какие из них «глухие», и понимает, на каком слое отказ.

---

## v0.1 — MVP (текущая итерация)

- [x] Processes tab: агрегат по ролям из `ps`/proctitle
- [x] Graphs tab: спарклайны CPU sum / mem% / load1
- [x] Logs tab: tail с подсветкой уровня + live-фильтр
- [x] Runtime control: hotkeys + модальное меню
- [x] SSH через системный `ssh` с ControlMaster
- [x] Unit-тесты парсеров

---

## v0.2a — устойчивость, channel-based event loop

Цель: **никогда не зависать и не показывать молча устаревшие данные**.

- [x] **Background refresh tasks** (отдельные tokio-таски на коллектор, канал
  в render-loop), чтобы медленный коллектор не задерживал быстрые. Реализовано
  в `src/source.rs` (spawn_collectors + три параллельных task).
- [x] **Backoff и retry** на сетевых ошибках SSH: экспоненциальный, cap 30s,
  reset на успехе. Юнит-тестами зафиксировано.
- [x] **Per-source бейджи в шапке**: `[ps ✓] 1s ago, 23ms`, `[sys ⚠] 14s ago,
  4200ms err×3`. Цвет — зелёный/жёлтый/красный по STALE/ERROR.
- [x] **STALE индикатор** (10 секунд без успешного ok).
- [x] **Force-refresh `r`** превратился в Notify всем коллекторам.
- [x] **Pause `p`**: receiver отбрасывает входящие, коллекторы продолжают
  крутиться — после анпауза картинка свежая сразу, не через полный интервал.

## v0.2b — streaming-логи через tail -F

Цель: **строки лога появляются мгновенно**, без поллинга. Главная видимая
польза в момент инцидента.

- [x] **`tail -n N -F` через long-lived ssh-подпроцесс**, чтение stdout
  построчно `tokio::io::BufReader::lines()` → `mpsc<CollectorMsg::LogStreamLine>`.
- [x] **Reconnect c backoff** при EOF/ошибке (экспоненциальный, cap 30s).
- [x] **Бейдж в шапке**: `[log ●] streaming, last 3s ago, reconn ×2` —
  отдельная семантика, не путаем «нет новых строк» (норма) с «стрим оборвался».
- [x] **Force-reconnect `L`**: ручной триггер через `Notify`, не ждём backoff.
- [x] **Cap буфера логов** на MAX_LOG_LINES (5000) с FIFO-обрезкой.

## v0.2c — russh long-lived session (отложено)

`russh` (pure-Rust SSH) даёт latency ~5 ms на exec против ~50-200 ms у
process-ssh. **Но** с ControlMaster (уже включён в v0.1) latency процесс-ssh
падает до 5-15 ms через unix-socket мультиплекс — marginal win. Возьмёмся,
только если профилинг покажет, что fork/exec ssh реально мешает.

Когда возьмёмся, в скоупе:
- [ ] Транспорт-абстракция (enum `Transport::Process | Russh`).
- [ ] Auth: SSH agent (через SSH_AUTH_SOCK) + identity file.
- [ ] Known hosts: первая итерация — `--insecure-host-key`, далее парсинг
  `~/.ssh/known_hosts`.
- [ ] Health-метрика «session up Xs, channels N» в шапке.

## v0.3 — `zabbix.stats` через trapper-порт ✅

Цель: **получить богатую внутреннюю телеметрию без зависимости от SSH** —
тот же канал, который сам Zabbix использует для самомониторинга.

Это запрос по бинарному Zabbix-протоколу на порт **TCP:10051**. Это **не**
HTTP API: PHP/Apache/база не задействованы, только сам процесс `zabbix_server`.
Авторизация — IP-ACL через `StatsAllowedIP` в `zabbix_server.conf` (никакого
токена). На клиенте — TCP + Zabbix header (`ZBXD` + flags + length) + JSON.

- [x] Новый модуль **`zbxstats.rs`**: framing (ZBXD\x01 + u32 LE +
  reserved), TCP к trapper-порту, парсер. Юнит-тесты на realistic
  payload + минимальный случай.
- [x] Запрос `{"request":"zabbix.stats"}` — version/uptime/hostname,
  process busy% (avg/max/min) по типам, wcache/rcache/vcache utilisation,
  queue, vps. queue/vps хранятся как `serde_json::Value` для гибкости
  между версиями Zabbix.
- [x] Новая панель **«Internals» (таб 4)**: server identity, queue, vps,
  таблица процессов с busy avg/max/min, gauge-кэши.
- [x] **Cross-compare** в Processes-таб: новые колонки `BusyPs`,
  `BusyZbx`, `Δ` — расхождение (busy от stats высокий, CPU/ps низкий)
  явный признак ожидания (lock/DB/IO), подсвечен красным.
- [x] **Source-indicator** в шапке: `[stats ✓] 1s ago, 45ms` рядом с
  остальными источниками.
- [x] CLI: `--stats-host`, `--stats-port` (default 10051), `--no-stats`.

Отложено в v0.3b:
- [ ] Запрос `{"request":"zabbix.stats","type":"queue","params":{...}}` —
  гистограмма очереди по бакетам задержки.
- [ ] TLS PSK/cert на trapper-порту (для setup-ов с защищённым trapper).
- [ ] Keep-alive: сейчас на каждый poll новый TCP-коннект (latency ~5-30 ms
  в датацентре). Если станет узким — переход на reuse через держим один
  открытый сокет.

**Граница с v0.2 `diaginfo`:** часть данных пересекается. Если stats endpoint
доступен, `diaginfo` нужен только для top-N items в historycache/valuecache,
которых нет в `stats`. Логика:
- если `stats` отвечает — берём cache/queue оттуда (дёшево, синхронно);
- `zabbix_server -R diaginfo` оставляем как ручной trigger (горячая клавиша),
  который пишет в лог расширенный дамп.

## v0.4 — DB-скрейпер (PostgreSQL) ✅

Цель: **отличить «болеет сам сервер» от «болеет БД»**, потому что для Zabbix
БД — самое типичное место отказа.

- [x] Модуль **`src/db.rs`**: `tokio-postgres` (NoTls), `DbTarget(url)`,
  `fetch_db_stats()` с таймаутами на каждый запрос.
- [x] Метрики PostgreSQL:
  - `pg_stat_activity`: connections by state (active/idle/idle-in-tx/waiting).
  - Top-10 long-running queries (по `now() - query_start`) с wait_event.
  - `pg_locks` waiting (count).
  - Размеры Zabbix-таблиц: `history*`, `trends*`, `events`, `event_recovery`,
    `problem` — сортировка по `pg_total_relation_size`.
  - Replication lag через `pg_is_in_recovery()` +
    `pg_last_xact_replay_timestamp()`.
  - `server_version`.
- [x] CLI: `--db-url postgres://...` (или env `ZTOP_DB_URL`). Если не задан,
  DB-источник выключен полностью.
- [x] Панель **«Database» (таб 5)**: summary с reps lag/locks, top queries
  с цветовым возрастом, ASCII-бары размеров таблиц.
- [x] Source-badge `[db ✓]` в шапке.

## v0.4b — алерт-аккорд + persistent PG ✅

Главное: смысл всей многослойной архитектуры — **пересечение сигналов**.
Каждое правило берёт ≥2 источника и ставит диагноз, который нельзя получить,
глядя на один таб.

- [x] Новый модуль **`src/diagnose.rs`**: `Severity`, `Diagnosis`,
  `diagnose(&App) -> Vec<Diagnosis>` с 6 правилами:
  - `history not landing in DB` (stats `history syncer busy > 85%` **И**
    DB queries on `history*` waiting / `connections.waiting > 0`)
  - `DB lock contention` (`locks_waiting > 0` или queries с `wait_event = Lock:*`)
  - `idle-in-transaction zombie` (idle-in-tx > 60s, блокирует housekeeper)
  - `queue grows while workers idle` (`queue > 1000` **И** все процессы < 30%)
  - `<role> waiting, not running` (Δ busy(zbx) - busy(ps) ≥ 30%)
  - `replica lag` (> 30s)
- [x] **UI-полоса диагнозов** между tabs и body, динамическая высота 0–3
  строки, цвета по severity (Critical/Warning/Info).
- [x] **Persistent PG connection**: `DbConnection` (Drop с abort), `spawn_db`
  держит `Option<DbConnection>`, переподключается только на ошибке. На
  localhost экономит ~3-5 ms на каждый poll.
- [x] Юнит-тесты для каждого правила (positive + healthy case).

## v0.4c — MySQL/MariaDB ✅

- [x] `mysql_async = "0.34"` (native-tls по дефолту, опция переключиться на
  rustls в Cargo.toml-комменте).
- [x] Общий **`DbBackend` enum** в `src/db.rs`: маршрутизация по схеме URL —
  `postgres://` / `postgresql://` → PG, `mysql://` → MySQL, без схемы → PG.
- [x] `MyConnection` с **try-or-default per-query**: каждый запрос обёрнут в
  `if let Ok(...)`, не существующая view или старый MySQL не валит fetch.
  Совместимо с MariaDB / MySQL 5.7 / 8.0+.
- [x] Запросы MySQL:
  - `SELECT VERSION()`
  - `information_schema.PROCESSLIST` (group by COMMAND+STATE) — маппинг на
    наши active/idle/waiting/other; `Sleep` = idle, `Query`+`Lock` = waiting.
  - Top-10 long queries из PROCESSLIST с `state.contains("Waiting")` →
    переносим в `wait_event` (грубое приближение PG-events).
  - `INNODB_LOCK_WAITS` (5.7) → fallback `performance_schema.data_lock_waits` (8.0+).
  - Размеры zabbix-таблиц через `information_schema.TABLES`.
  - `SHOW REPLICA STATUS` (8.0.22+) → fallback `SHOW SLAVE STATUS`, читаем
    `Seconds_Behind_Source` или `Seconds_Behind_Master`.
- [x] Поле `DbStats.backend` ("postgres"/"mysql") — UI показывает корректную
  метку в шапке таба Database.

## v0.4c.1 — MySQL parity с PostgreSQL ✅

- [x] **Точный `wait_event`** через JOIN `information_schema.PROCESSLIST` +
  `performance_schema.threads` + `performance_schema.events_waits_current`.
  Если perf_schema выключен или нет прав — try-or-default fallback на
  простую PROCESSLIST (как было в v0.4c).
- [x] **idle-in-transaction** через `information_schema.innodb_trx`:
  - Counts → `connections.idle_in_transaction` (с вычитанием из `idle`,
    чтобы не дублировать — PROCESSLIST показывает их как Sleep).
  - Зомби с age ≥ 60s → синтетические `LongQuery` записи с
    `state = "idle in transaction"`. Это позволяет существующему
    алерт-правилу `rule_idle_in_tx_zombie` сработать для MySQL тоже.

## v0.4d — TLS ✅ (pgpass отложен)

- [x] **TLS до PG** через `postgres-native-tls` + `native-tls`. Не feature-
  gated, а просто включён — `sslmode=disable` в URL обходит TLS без
  накладных расходов. MySQL TLS работал из коробки через mysql_async.
- [x] **`--db-insecure-tls`** (env `ZTOP_DB_INSECURE_TLS`) — для self-signed
  и приватных CA. Отдельный флаг, чтобы случайно не отключить верификацию
  на проде.
- [x] SCRAM-SHA-256 работает из коробки в `tokio-postgres` — отдельной
  поддержки не требуется (включено в его default features).

## v0.4d.1 — pgpass / my.cnf ✅

- [x] Новый модуль **`src/dbcreds.rs`**:
  - `enrich_pg_url(url) -> String` / `enrich_my_url(url) -> String`
  - Парсер URL (scheme/user/pass/host/port/db/query), сборка обратно с
    percent-encoded паролем.
  - Чтение **`~/.pgpass`** или `$PGPASSFILE`. Проверка прав файла
    (Unix: ≤ 0600). Поддержка `*` для любого поля, backslash-escape `:` и
    `\\` в полях.
  - Чтение **`~/.my.cnf`** секций `[client]`, `[mysql]`, `[client-server]`.
    Поддержка quoted-значений. Если в my.cnf user отличается от URL —
    пароль не подставляем (защита от двойной идентичности).
- [x] Подключение в `main.rs`: enrich URL перед `DbTarget::new` по схеме.
  Если URL уже с паролем — обогащение пропускается, URL остаётся как есть.
- [x] Юнит-тесты: парсинг URL, rebuild с percent-encoding, split pgpass с
  escape, парсинг my.cnf секций, pct round-trip.

## v0.5a — мультихост (MVP) ✅

- [x] Конфиг **`~/.config/ztop/hosts.toml`** (или явный `--config <path>`),
  TOML с массивом `[[host]]`. Single-host через CLI остаётся как fallback,
  если конфига нет.
- [x] Извлечён **`HostState`** из `App`. `App.hosts: Vec<HostState>` +
  `focused_host: usize`. Все per-host данные (procs/sys/logs/stats/db/
  history/sources/log_stream) переехали в HostState.
- [x] **`HostMsg { host_idx, msg }`** — каждый коллектор оборачивает
  CollectorMsg в HostMsg перед отправкой. Event-loop маршрутизирует по
  индексу: `app.hosts.get_mut(host_msg.host_idx).apply_msg(...)`.
- [x] Новый таб **`0 Overview`** с health-светофором по всем хостам:
  кол-во здоровых источников из всего, CPU/Mem/Load/Queue, кол-во активных
  диагнозов, маркер фокуса.
- [x] **Ctrl-N / Ctrl-P** — переключение фокуса между хостами (работает
  на любом табе). Имя текущего хоста показывается в шапке и табах.
- [x] **Diagnoses панель — теперь по всем хостам** с префиксом
  `[hostname]`. Top-3 most-severe across the fleet.
- [x] Runtime control (`+/-/c/h/d/R`) выполняется на focused host;
  toast тоже префиксуется именем хоста.

## v0.5b — добивка мультихоста ✅

- [x] **`zabbix_proxy:` proctitle** — `TITLE` regex и awk-фильтр в
  `fetch_procs` теперь принимают и `server`, и `proxy`. Тот же парсер,
  те же роли (poller/trapper/history syncer/...). Прокси-specific роли
  (data sender, heartbeat sender, vmware collector) распознаются
  автоматически — regex не привязан к whitelist-у имён.
- [x] **Cursor navigation на Overview**: `↑`/`↓` (или `k`/`j`) двигают
  курсор по строкам, **Enter** — переключает focused_host и сразу уходит
  на drill-down (Tab::Processes). При входе на Overview через `0` курсор
  синхронизируется с текущим focused_host. Два маркера:
    - `▸` — курсор (где сейчас выбор),
    - `●` — focused (какой хост открыт в drill-down табах).
- [x] **Aggregate-панель** под таблицей Overview — fleet-level метрики:
    - Σ CPU (сумма по всем хостам), max load1, mem avg/max%, healthy ratio
    - Σ queue (сумма размеров очередей), общий счётчик диагнозов с
      выделением critical отдельно.

## v0.5c — крупные деплои (modal switcher) ✅

- [x] **Modal host-switcher** с fuzzy-search по `Ctrl-G`. Overlay 60%×60%
  с полем запроса, счётчиком «N/M match» и ранжированным списком.
- [x] **Fuzzy subsequence matcher** (`app::fuzzy_score`): score за каждое
  совпадение + бонус за consecutive runs + бонус за start-of-string.
  «prod» лучше матчит «zbx-prod-01», чем разбросанные «p…r…o…d».
- [x] **Поиск по двум полям**: `name` и `ssh_host` — берётся max score из них.
- [x] **Два маркера курсора**: `▸` (где сейчас выбор), `●` (focused host),
  combo `▸●`.
- [x] Юнит-тесты на subsequence/consecutive/empty-query.

## v0.5d — оставшиеся идеи для крупных деплоев

- [ ] **Группировка** хостов по environment/region/datacenter в Overview
  (sub-headers в таблице).
- [ ] **Saved layouts**: запомнить состояние (focused host, tab, paused)
  между сессиями в `~/.cache/ztop/state.json`.
- [ ] **Параллелизация конфиг-load**: сейчас `DbBackend::connect`
  последовательный при старте, при 30+ хостах долго; нужен `join_all`.
- [ ] **Highlight matched chars** в host picker — подсвечивать жёлтым
  буквы, попавшие в subsequence.

## v0.6 — запись и post-mortem ✅

Цель: **«вчера в 03:14 был сбой»** — анализировать после факта.

- [x] Новый модуль **`src/record.rs`** с JSONL-форматом:
  - `HeaderLine` (версия, started_at, список HostMeta) — первая строка.
  - `EventLine { ts, host_idx, msg }` — на каждый `HostMsg` записывается строка.
  - `RecordedMsg` — сериализуемый mirror `CollectorMsg` (anyhow::Error → String).
  - Все state types получили `#[derive(Serialize, Deserialize)]`.
- [x] **`--record path.jsonl`**: open file, write header с хостами,
  recorder.write вызывается в event-loop до apply_msg. BufWriter
  батчит, Drop флашит.
- [x] **`--replay path.jsonl`**: read_header → reconstruct HostState
  list → spawn replay_task который шлёт HostMsg в тот же mpsc-канал
  с задержками по оригинальным timestamps. Speed override через
  `--replay-speed` (1.0 default, >1 — быстрее).
- [x] В replay-режиме **runtime control заблокирован** (toast «disabled
  in --replay mode»), refresh/reconnect — заглушки.
- [x] **UI индикатор** в шапке: `● REC (N ev)` красным или `▶ REPLAY` синим.
- [x] **Диагнозы работают в replay** автоматически — они построены поверх
  HostState, который восстанавливается из событий. Можно перематывать запись
  инцидента и видеть когда какой `[CRIT] history not landing in DB` загорелся.

## v0.6.1 — интерактивный replay ✅

- [x] **`Space` — pause/resume**: replay_loop через `tokio::select!` с
  условной веткой `if !paused` на sleep; команды управления через
  unbounded mpsc-канал.
- [x] **`n` — step**: продвигает на один event (имеет смысл в paused).
- [x] **`>` / `.` — seek forward 60s**: cursor через `partition_point`
  по timestamp; anchor-ы пересчитываются.
- [x] **`<` / `,` — seek backward 60s**: cursor назад + **синтетический
  `CollectorMsg::Reset`** для всех хостов, иначе UI показывал бы данные
  из будущего. State перестраивается естественно по входящим событиям.
- [x] In-memory load всей записи (`load_recording`) — позволяет seek в
  обе стороны. Lazy-streaming как было в v0.6 поддерживал только forward.
- [x] **UI прогресс**: `▶ REPLAY 45.2%` или `⏸ REPLAY 45.2% [PAUSED]`,
  атомарный счётчик 0..=1000 промилле обновляется replay_loop-ом.

## v0.6.2 — постмортем pro (отложено)

- [ ] **Compare-режим**: два временных окна рядом, дифф по ролям.
- [ ] **Экспорт экрана** в `ztop-YYYYMMDD-HHMMSS.txt` хоткеем.
- [ ] **Compaction**: `ztop --compact src.jsonl dst.jsonl` убирает
  избыточные LogStreamLine между ps-snapshot-ами.
- [ ] **Seek-to-diagnose**: прыжок на момент когда конкретный
  Diagnosis впервые сработал.
- [ ] **Bookmarks**: метки на интересных моментах в записи (`b` ставит,
  `B` — список).

## v0.7 — диагностические пробы ✅

Цель: **отделить «болеет сам Zabbix» от «болеет DB/сеть/упстрим»**. Пробы
запускаются **с машины ztop**, что позволяет сразу видеть, лагает ли это
конкретно мой ноутбук/сеть или реально болеет сервер/БД.

- [x] Новый модуль **`src/probes.rs`**: `ProbeConfig`, `ProbeState`,
  `ProbeMsg`, `spawn_probes`. Каждая проба — отдельная tokio-задача с
  собственным интервалом и таймаутом.
- [x] **TCP probe**: `TcpStream::connect(host:port)` с timing.
- [x] **DNS probe**: `tokio::net::lookup_host` resolve latency.
- [x] **PG probe**: открыть коннект (`tokio_postgres::connect`),
  `SELECT 1`, закрыть. Через `dbcreds::enrich_pg_url`, так что пароли
  из ~/.pgpass работают.
- [x] **Конфиг**: блоки `[[probe]]` в том же `hosts.toml` с полем `kind`.
- [x] **Новый таб `6 Probes`**: таблица name/kind/target/latency/status/
  success%/last_error. Цветовое кодирование latency (зелёный <100ms,
  жёлтый <1s, красный >1s) и success% (>99% зелёный, >90% жёлтый, иначе
  красный). Пароли в URL маскируются `****`.
- [x] **Opt-in**: без `[[probe]]` блоков таб показывает hint с примером.

Отложено в v0.7.1:
- [ ] **TLS handshake probe** через `tokio-native-tls`.
- [ ] **MySQL probe** через `mysql_async`.
- [ ] **HTTP probe**: GET с status code и timing (понадобится `reqwest`).
- [ ] **Гистограмма** latency за N сек (percentiles).
- [ ] **Cross-probe diagnose-правила**: «PG ping ok, но DB-источник в
  Zabbix-хосте падает — проблема либо в network между хостами, либо в
  правах пользователя».

## v0.8+ — полировка

- [ ] Help-overlay по `?`, hotkey-cheatsheet.
- [ ] Конфиг-файл с переопределением hotkeys, цветов, порогов.
- [ ] Theme: dark/light/colorblind preset.
- [ ] Локализация (RU/EN), сейчас английский в коде.
- [ ] Дистрибуция: prebuilt binaries в release, Homebrew tap, deb/rpm,
  docker image для CI-проб.

---

## Testbed / dev infrastructure ✅

- [x] **`docker/`** — full Zabbix stack для отладки ztop:
  - PG + zabbix-server (custom image c sshd) + agent + web frontend
  - SSH-keys генерируются `setup.sh`, sudoers для runtime control
  - `hosts.toml` готовый — `ztop --config docker/hosts.toml` и поехали
- [x] **6 stress-сценариев** в `docker/stress/`:
  - `stress_trapper.sh` — bulk values через `zabbix_sender`
  - `stress_api.sh` — curl-loop на JSON-RPC API
  - `stress_db_slow.sh` — параллельные `SELECT pg_sleep(N)`
  - `stress_db_zombie_tx.sh` — idle-in-transaction симулятор
  - `stress_log_spam.sh` — ramp log_level для проверки log streaming
  - `stress_history_flood.sh` — **combo для поджига `[CRIT] history not
    landing in DB`**, главного diagnose-правила
- [x] Каждый stress-script задокументирован что должно зажечься в ztop.
  Это превращает ручную проверку в «запусти и убедись».

Что отложено:
- [ ] **CI** smoke-test: GitHub Actions поднимает testbed, прогоняет stress
  сценарии, читает прогресс через record/replay, сверяет что нужные
  диагнозы загорелись. Это превращает stress-сценарии в e2e-тесты.

## Сквозные задачи (не привязаны к версии)

- [ ] CI на GitHub Actions: `cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo test`, build artifacts для Linux/macOS.
- [ ] Бенчмарки парсеров (`criterion`) — чтобы 1000-форк хосты не лагали.
- [ ] Man page (через `clap_mangen`).
- [ ] Asciinema-демо в README.
- [ ] Tracing/logs из самого ztop в `~/.cache/ztop/ztop.log`, чтобы диагностить
  свои же зависания.

---

## Что осознанно НЕ делаем

- **Не интегрируемся с Zabbix HTTP API (`api_jsonrpc.php`).** Принципиально —
  он зависит от PHP/Apache/БД, а это самые ломкие звенья. Внутренние
  метрики берём через `zabbix.stats` на trapper-порту (см. v0.3) — это
  тот же бинарный протокол, что и у Zabbix-агентов, без PHP-стека.
- **Не пишем агент для самого Zabbix.** ztop — внешний наблюдатель, не
  data source.
- **Не делаем web-UI/HTTP endpoint.** Это конкуренция с самим Zabbix-фронтом,
  не цель проекта.
- **Не пишем собственный SSH-стек с нуля.** russh — да, libssh2 — да, свой —
  нет.

---

## Решения «когда возьмёмся»

Это не приоритеты, а контрольные точки — что будет триггером взять задачу
в работу.

| Триггер                                              | Задача             |
|------------------------------------------------------|--------------------|
| Если ztop сам начнёт тормозить на больших инсталляциях | v0.2 long SSH      |
| Если в логах что-то критичное проскочит между poll-тиками | v0.2 tail -F        |
| Если будет инцидент с busy% сильно ≠ CPU%             | v0.3 zabbix.stats  |
| Если поймали инцидент «база лежит, сервер живой»      | v0.4 DB scraper    |
| Если появится второй Zabbix-сервер в управлении        | v0.5 multi-host    |
| Если придётся писать постмортем без полной телеметрии  | v0.6 record/replay |
