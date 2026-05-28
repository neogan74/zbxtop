#!/bin/bash
# entrypoint.sh — sshd в фоне + оригинальный zabbix-entrypoint на foreground.
#
# Если /etc/ztop-ssh-keys/id_ed25519.pub смонтирован — подкладываем его в
# authorized_keys пользователя ztop. setup.sh из docker/ генерирует этот ключ.

set -e

if [ -f /etc/ztop-ssh-keys/id_ed25519.pub ]; then
    install -m 700 -o ztop -g ztop -d /home/ztop/.ssh
    install -m 600 -o ztop -g ztop /etc/ztop-ssh-keys/id_ed25519.pub /home/ztop/.ssh/authorized_keys
    echo "[entrypoint] ztop SSH key installed"
else
    echo "[entrypoint] WARNING: ssh-keys/id_ed25519.pub not mounted, ssh login disabled"
fi

# Гарантируем, что host keys существуют (на первом старте sshd сам их создаёт,
# но иногда официальный image без них — генерируем явно).
ssh-keygen -A >/dev/null 2>&1 || true

# sshd в фоне
/usr/sbin/sshd -D &
SSHD_PID=$!
echo "[entrypoint] sshd started (pid=$SSHD_PID)"

# Грейсфул-shutdown: пробрасываем SIGTERM в zabbix_server (sshd погаснет вместе с контейнером).
trap 'echo "[entrypoint] stopping..."; kill -TERM $ZBX_PID 2>/dev/null; wait $ZBX_PID' SIGTERM SIGINT

# Запускаем оригинальный docker-entrypoint.sh от zabbix-server-pgsql.
/usr/bin/docker-entrypoint.sh /usr/sbin/zabbix_server --foreground &
ZBX_PID=$!
echo "[entrypoint] zabbix_server started (pid=$ZBX_PID)"

wait $ZBX_PID
