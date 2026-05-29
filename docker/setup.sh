#!/bin/bash
# setup.sh — one-time testbed preparation:
#   1. Generates an SSH key for the ztop user inside the container.
#   2. Prints the ~/.ssh/config block that must be added manually.
#   3. Prints the connection command.

set -e
cd "$(dirname "$0")"

KEY_DIR="ssh-keys"
KEY="$KEY_DIR/id_ed25519"

mkdir -p "$KEY_DIR"

if [ ! -f "$KEY" ]; then
    ssh-keygen -t ed25519 -N "" -C "ztop testbed" -f "$KEY" -q
    echo "[setup] Generated $KEY"
else
    echo "[setup] $KEY already exists, leaving as is"
fi

chmod 700 "$KEY_DIR"
chmod 600 "$KEY"
chmod 644 "$KEY.pub"

ABSKEY="$(cd "$KEY_DIR" && pwd)/id_ed25519"

cat <<EOF

[setup] Done. Next steps:

1. Append this block to ~/.ssh/config (or merge with existing):

    Host ztop-testbed
        HostName 127.0.0.1
        Port 22022
        User ztop
        IdentityFile $ABSKEY
        IdentitiesOnly yes
        UserKnownHostsFile /dev/null
        StrictHostKeyChecking no

2. Build and start the stack:

    cd docker && docker compose up -d --build

3. Wait ~30s for Zabbix to initialize the DB. Check:

    docker compose ps
    docker compose logs zabbix-server | tail -20

4. Connect ztop:

    # From repo root
    cargo run --release -- --config docker/hosts.toml

   Web UI (default Admin/zabbix):  http://127.0.0.1:8088

5. Stress scenarios (in separate terminals):

    ./stress/stress_trapper.sh 1000 60      # 1000 vals/sec, 60 sec
    ./stress/stress_db_zombie_tx.sh 300     # idle-in-tx for 5 min
    ./stress/stress_db_slow.sh              # slow SELECTs
    ./stress/stress_api.sh 60               # API spam 60 sec
    ./stress/stress_log_spam.sh             # ramp up log_level

   In ztop observe: the Diagnoses strip, tab 4 Internals (busy syncer),
   tab 5 Database (locks_waiting, idle_in_transaction, top_queries).

EOF
