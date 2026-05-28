#!/bin/bash
# stress_api.sh — спам по Zabbix JSON-RPC API.
#
# Бьёт по zabbix-web (PHP) на порту 8088. Нагружает PHP-стэк и БД (через
# веб). Запускает несколько параллельных curl-ов в цикле.
#
# Что должно произойти в ztop:
#   - на табе 5 Database: connections.active растёт
#   - возможно top_queries наполнится SELECT-ами из PHP-фронта
#   - probe Web frontend может покраснеть если PHP захлёбывается
#
# Использование:
#   ./stress_api.sh [DURATION_SEC] [CONCURRENCY]
#
# Дефолт: 60 сек, 5 параллельных потоков.

set -e
DUR=${1:-60}
CONC=${2:-5}
URL="http://127.0.0.1:8088/api_jsonrpc.php"

if ! curl -sf -o /dev/null "$URL"; then
    echo "[api-stress] ERROR: $URL is not reachable"
    echo "Hint: zabbix-web нужен ~30s после старта. Подожди или проверь:"
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
