#!/bin/bash
# entrypoint.sh — sshd in the background + original zabbix entrypoint in the foreground.
#
# If /etc/ztop-ssh-keys/id_ed25519.pub is mounted, it is placed into
# authorized_keys for the ztop user. setup.sh in docker/ generates this key.

set -e

if [ -f /etc/ztop-ssh-keys/id_ed25519.pub ]; then
    install -m 700 -o ztop -g ztop -d /home/ztop/.ssh
    install -m 600 -o ztop -g ztop /etc/ztop-ssh-keys/id_ed25519.pub /home/ztop/.ssh/authorized_keys
    echo "[entrypoint] ztop SSH key installed"
else
    echo "[entrypoint] WARNING: ssh-keys/id_ed25519.pub not mounted, ssh login disabled"
fi

# Ensure host keys exist (sshd creates them on first start, but sometimes
# the official image ships without them — generate explicitly).
ssh-keygen -A >/dev/null 2>&1 || true

# sshd in the background
/usr/sbin/sshd -D &
SSHD_PID=$!
echo "[entrypoint] sshd started (pid=$SSHD_PID)"

# Graceful shutdown: forward SIGTERM to zabbix_server (sshd will die with the container).
trap 'echo "[entrypoint] stopping..."; kill -TERM $ZBX_PID 2>/dev/null; wait $ZBX_PID' SIGTERM SIGINT

# Start the original docker-entrypoint.sh from zabbix-server-pgsql.
/usr/bin/docker-entrypoint.sh /usr/sbin/zabbix_server --foreground &
ZBX_PID=$!
echo "[entrypoint] zabbix_server started (pid=$ZBX_PID)"

wait $ZBX_PID
