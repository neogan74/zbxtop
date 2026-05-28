#!/bin/bash
# stress_log_spam.sh — повышаем log level zabbix_server несколько раз.
# Это разворачивает DEBUG-логи: десятки строк в секунду в zabbix_server.log.
#
# Что должно произойти в ztop:
#   - таб «3 Logs» льёт строки real-time (streaming, не поллингом)
#   - badge [log ●] остаётся «streaming, last 0s ago»
#   - можно ставить фильтр / на специфический worker
#   - после стресс-теста: log_level_decrease вернуть обратно
#
# Использование:
#   ./stress_log_spam.sh up    # ramp up до 5 (DEBUG)
#   ./stress_log_spam.sh down  # вернуть к дефолту
#
# Дефолтное действие: up.

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
