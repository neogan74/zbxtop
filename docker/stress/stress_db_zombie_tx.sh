#!/bin/bash
# stress_db_zombie_tx.sh — открывает транзакцию и держит её открытой N сек
# без активного запроса. Это classic «idle in transaction», который блокирует
# housekeeper и autovacuum в проде Zabbix.
#
# В ztop должно зажечься правило rule_idle_in_tx_zombie (для PG):
#   [WARN] idle-in-transaction zombie — 1 backends stuck (oldest 65s) ...
# При >=600 сек → severity Critical.
#
# Использование:
#   ./stress_db_zombie_tx.sh [DURATION_SEC]
#
# Дефолт: 300 сек (5 минут — Warning через минуту, Critical через 10).
# Ctrl-C прерывает.

set -e
DUR=${1:-300}

echo "[zombie-tx] Holding idle-in-transaction for ${DUR}s. Ctrl-C to abort."

# Трюк: psql читает stdin построчно. BEGIN, SELECT, потом долгий sleep
# с пустым stdin держит сессию открытой и в состоянии «idle in transaction».
(
    echo "BEGIN;"
    echo "SELECT now() AS started, pg_backend_pid() AS pid;"
    # Висим. Когда sleep закончится — пойдут команды ниже.
    sleep "$DUR"
    echo "ROLLBACK;"
) | docker compose exec -T postgres psql -U zabbix -d zabbix
echo "[zombie-tx] Transaction released"
