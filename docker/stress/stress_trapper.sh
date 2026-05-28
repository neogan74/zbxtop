#!/bin/bash
# stress_trapper.sh — массовая отправка values в trapper.
#
# Использует zabbix_sender внутри контейнера zabbix-agent. Items не обязаны
# существовать — даже rejected values нагружают trapper-процессы (видно в
# таб «4 Internals» как busy trapper и в «1 Processes» как busy proctitle).
#
# Что должно произойти в ztop:
#   - busy% у trapper в Internals растёт
#   - vps растёт
#   - возможно сработает rule_busy_cpu_delta если busy(zbx) > busy(ps)
#
# Использование:
#   ./stress_trapper.sh [VALUES_PER_BATCH] [DURATION_SEC]
#
# Дефолт: 500 values, 60 сек.

set -e
VPB=${1:-500}
DUR=${2:-60}

echo "[trapper-stress] $VPB values/batch, 1 batch/sec for ${DUR}s"
echo "[trapper-stress] Server: zabbix-server:10051"

end_time=$(($(date +%s) + DUR))
i=0
while [ "$(date +%s)" -lt $end_time ]; do
    # Генерируем batch и шлём его через stdin zabbix_sender.
    {
        for j in $(seq 1 "$VPB"); do
            echo "ztop-test test.metric.$((j % 50)) $((RANDOM % 1000))"
        done
    } | docker compose exec -T zabbix-agent zabbix_sender \
        -z zabbix-server -p 10051 -i - 2>/dev/null \
        | tail -1
    i=$((i + 1))
    sleep 1
done
echo "[trapper-stress] Done after ${i} batches"
