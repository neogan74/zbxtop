#!/bin/bash
# stress_log_spam.sh — raise the zabbix_server log level several times.
# This enables DEBUG logs: dozens of lines per second in zabbix_server.log.
#
# What should happen in ztop:
#   - tab "3 Logs" streams lines in real-time (streaming, not polling)
#   - badge [log ●] stays "streaming, last 0s ago"
#   - you can set a filter / for a specific worker
#   - after the stress test: run log_level_decrease to restore the level
#
# Usage:
#   ./stress_log_spam.sh up    # ramp up to 5 (DEBUG)
#   ./stress_log_spam.sh down  # restore to default
#
# Default action: up.

set -e
ACTION=${1:-up}

case "$ACTION" in
    up)
        echo "[log-spam] Ramping log_level UP 5 times (default 3 → ~8/clamped)"
        for i in 1 2 3 4 5; do
            docker compose exec -T zabbix-server \
                sudo -u zabbix /usr/sbin/zabbix_server -R log_level_increase \
                2>&1 | head -1
            sleep 0.3
        done
        echo "[log-spam] Done. Check ztop tab '3 Logs' — should be flooded with DEBUG."
        echo "[log-spam] Stop with: $0 down"
        ;;
    down)
        echo "[log-spam] Ramping log_level DOWN 5 times"
        for i in 1 2 3 4 5; do
            docker compose exec -T zabbix-server \
                sudo -u zabbix /usr/sbin/zabbix_server -R log_level_decrease \
                2>&1 | head -1
            sleep 0.3
        done
        echo "[log-spam] Done."
        ;;
    *)
        echo "usage: $0 {up|down}"
        exit 1
        ;;
esac
