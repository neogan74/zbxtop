#!/bin/bash
# stress_db_slow.sh — heavy SELECTs with pg_sleep to populate
# top_queries in the ztop tab "5 Database".
#
# What should happen in ztop:
#   - top_queries shows SELECT pg_sleep(...) with growing age
#   - connections.active increases
#   - wait_event = "Lock:..." or "IO:..." may appear
#
# Usage:
#   ./stress_db_slow.sh [DURATION_SEC] [CONCURRENCY]
#
# Default: 60 sec, 3 parallel queries.

set -e
DUR=${1:-60}
CONC=${2:-3}

echo "[db-slow] $CONC parallel slow queries for ${DUR}s"
end_time=$(($(date +%s) + DUR))

slow_worker() {
    local id=$1
    while [ "$(date +%s)" -lt $end_time ]; do
        docker compose exec -T postgres psql -U zabbix -d zabbix -c \
            "SELECT pg_sleep(${id}.5), 'worker-${id}', count(*) FROM pg_class;" \
            > /dev/null 2>&1 || true
    done
    echo "[db-slow] worker $id done"
}

for i in $(seq 1 "$CONC"); do
    slow_worker "$i" &
done
wait
echo "[db-slow] All workers done"
