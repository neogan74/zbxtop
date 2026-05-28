#!/bin/bash
# stress_db_slow.sh — тяжёлые SELECT-ы с pg_sleep, чтобы заполнить
# top_queries в ztop таб «5 Database».
#
# Что должно произойти в ztop:
#   - top_queries показывает SELECT pg_sleep(...) с age растущим
#   - connections.active увеличивается
#   - возможно появится wait_event = "Lock:..." или "IO:..."
#
# Использование:
#   ./stress_db_slow.sh [DURATION_SEC] [CONCURRENCY]
#
# Дефолт: 60 сек, 3 параллельных запроса.

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
