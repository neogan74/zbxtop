#!/bin/bash
# stress_db_zombie_tx.sh — opens a transaction and holds it idle for N seconds
# without an active query. This is the classic "idle in transaction" state that
# blocks housekeeper and autovacuum in a production Zabbix setup.
#
# In ztop the rule rule_idle_in_tx_zombie should fire (for PG):
#   [WARN] idle-in-transaction zombie — 1 backends stuck (oldest 65s) ...
# At >=600 sec → severity Critical.
#
# Usage:
#   ./stress_db_zombie_tx.sh [DURATION_SEC]
#
# Default: 300 sec (5 minutes — Warning after one minute, Critical after ten).
# Ctrl-C to abort.

set -e
DUR=${1:-300}

echo "[zombie-tx] Holding idle-in-transaction for ${DUR}s. Ctrl-C to abort."

# Trick: psql reads stdin line by line. BEGIN, SELECT, then a long sleep
# with empty stdin keeps the session open and in the "idle in transaction" state.
(
    echo "BEGIN;"
    echo "SELECT now() AS started, pg_backend_pid() AS pid;"
    # Висим. Когда sleep закончится — пойдут команды ниже.
    sleep "$DUR"
    echo "ROLLBACK;"
) | docker compose exec -T postgres psql -U zabbix -d zabbix
echo "[zombie-tx] Transaction released"
