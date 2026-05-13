# ztop

Консольный (TUI) монитор внутреннего состояния **zabbix_server** по SSH.
Цель — наблюдать за процессами и логами Zabbix-сервера тогда, когда веб-UI/API
тормозят или недоступны: данные собираются напрямую из ОС (`ps`, `/proc`, log
tail) и через runtime-control команды самого `zabbix_server`.

## Что показывает

- **Processes** — таблица агрегатов по ролям форков `zabbix_server` (poller,
  trapper, history syncer, preprocessing worker, lld manager, escalator, ...):
  количество, суммарный CPU%, RSS, доля busy, последний статус из proctitle.
- **Graphs** — ASCII-спарклайны последних ~8 минут: суммарный CPU форков,
  используемая память, loadavg(1m).
- **Logs** — хвост `zabbix_server.log` с подсветкой уровня (error/warning/info)
  и live-фильтром.
- **Runtime control** — горячие клавиши и модальное меню для команд
  `zabbix_server -R`: log_level_increase/decrease, config_cache_reload,
  snmp_cache_reload, housekeeper_execute, diaginfo.

API Zabbix **не используется принципиально** — инструмент должен работать,
когда сама база/UI/API тормозят.

## Сборка

```
cargo build --release
```

Готовый бинарь — `target/release/ztop` (~3–5 МБ, статически линкованный
большинством зависимостей).

## Запуск

```
ztop --host zbx-prod-01 \
     --log  /var/log/zabbix/zabbix_server.log \
     --sudo                 # опционально, см. ниже
```

Все флаги дублируются переменными окружения: `ZTOP_HOST`, `ZTOP_LOG`,
`ZTOP_SUDO`.

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
  main.rs          — CLI, terminal setup, async event loop
  app.rs           — состояние, кольцевые буферы истории
  ssh.rs           — обёртка вокруг system ssh (tokio::process)
  collectors.rs    — парсеры ps/loadavg/meminfo/uptime/logs + runtime control
  ui.rs            — ratatui-рендеринг (header, tabs, табы, модал)
```

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
