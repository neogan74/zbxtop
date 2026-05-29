#!/bin/bash
# stress_api.sh — Zabbix JSON-RPC API spam.
#
# Hammers zabbix-web (PHP) on port 8088. Loads the PHP stack and DB (via web).
# Runs several parallel curl loops.
#
# What should happen in ztop:
#   - in tab 5 Database: connections.active grows
#   - top_queries may fill with SELECTs from the PHP frontend
#   - probe Web frontend may go red if PHP is overwhelmed
#
# Usage:
#   ./stress_api.sh [DURATION_SEC] [CONCURRENCY]
#
# Default: 60 sec, 5 parallel threads.

set -e
DUR=${1:-60}
CONC=${2:-5}
URL="http://127.0.0.1:8088/api_jsonrpc.php"

if ! curl -sf -o /dev/null "$URL"; then
    echo "[api-stress] ERROR: $URL is not reachable"
    echo "Hint: zabbix-web needs ~30s after startup. Wait or check:"
    echo "  docker compose ps"
    exit 1
fi

echo "[api-stress] $CONC threads × ${DUR}s on $URL"
end_time=$(($(date +%s) + DUR))

worker() {
    local id=$1
    local count=0
    while [ "$(date +%s)" -lt $end_time ]; do
        curl -s -H "Content-Type: application/json-rpc" \
            -d '{"jsonrpc":"2.0","method":"apiinfo.version","id":1,"params":{}}' \
            "$URL" > /dev/null || true
        count=$((count + 1))
    done
    echo "[api-stress] worker $id done: $count requests"
}

for i in $(seq 1 "$CONC"); do
    worker "$i" &
done
wait
echo "[api-stress] All workers done"
