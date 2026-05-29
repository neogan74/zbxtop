#!/bin/bash
# stress_trapper.sh — mass value submission to the trapper.
#
# Uses zabbix_sender inside the zabbix-agent container. Items do not have to
# exist — even rejected values load the trapper processes (visible in
# tab "4 Internals" as busy trapper and in "1 Processes" as busy proctitle).
#
# What should happen in ztop:
#   - busy% for trapper in Internals grows
#   - vps grows
#   - rule_busy_cpu_delta may fire if busy(zbx) > busy(ps)
#
# Usage:
#   ./stress_trapper.sh [VALUES_PER_BATCH] [DURATION_SEC]
#
# Default: 500 values, 60 sec.

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
