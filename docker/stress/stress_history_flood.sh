#!/bin/bash
# stress_history_flood.sh — combo scenario: trapper-flood + zombie-tx.
# Goal: trigger the rule rule_history_backed_up:
#   [CRIT] history not landing in DB — history syncer busy 95%
#          + N queries on history* waiting (DB conn waiting=K)  (stats+db)
#
# What it does:
#   1. Starts stress_db_zombie_tx in the background (for 5 min), loading autovacuum.
#   2. After 5 sec starts a heavy trapper-flood — many values at once.
#   3. Cleanup via trap.
#
# Usage:
#   ./stress_history_flood.sh [TRAPPER_DURATION]
#
# Default: 90 sec trapper-flood (zombie-tx runs for 300 sec).

set -e
cd "$(dirname "$0")"

TRAPPER_DUR=${1:-90}

echo "[history-flood] Starting zombie tx (background, 300s)..."
./stress_db_zombie_tx.sh 300 &
ZOMBIE_PID=$!

cleanup() {
    echo "[history-flood] Cleanup..."
    kill $ZOMBIE_PID 2>/dev/null || true
    # Try to release zombie tx
    docker compose exec -T postgres psql -U zabbix -d zabbix -c "SELECT pg_cancel_backend(pid) FROM pg_stat_activity WHERE state = 'idle in transaction' AND application_name = 'psql';" 2>/dev/null || true
}
trap cleanup EXIT

echo "[history-flood] Waiting 5s for zombie tx to settle..."
sleep 5

echo "[history-flood] Starting trapper flood for ${TRAPPER_DUR}s..."
./stress_trapper.sh 2000 "$TRAPPER_DUR"

echo "[history-flood] Done. Releasing zombie tx via cleanup..."
