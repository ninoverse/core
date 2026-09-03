#!/bin/bash
# Creates the SCRAM users and ACLs that live beyond the `admin` superuser
# RP_BOOTSTRAP_USER makes on first boot. Safe to re-run: user creation and ACL
# creation are both idempotent in redpanda.
#
# Start the broker first:
#   RP_ADMIN_PASSWORD=... ./bin/start_redpanda.sh
#
# Required:
#   RP_ADMIN_PASSWORD   the superuser password used to start the broker
#   CORE_APP_PASSWORD   the password to give the core app's own user
set -eu

CONTAINER="kafka"
APP_USER="core-app"
APP_TOPIC="ninoverse"
APP_GROUP="ninoverse"

: "${RP_ADMIN_PASSWORD:?set RP_ADMIN_PASSWORD (the admin superuser password)}"
: "${CORE_APP_PASSWORD:?set CORE_APP_PASSWORD (the password for the $APP_USER user)}"

# rpk authenticates as the superuser for every call below.
rpk() {
    docker exec \
        -e RPK_USER=admin \
        -e RPK_PASS="$RP_ADMIN_PASSWORD" \
        -e RPK_SASL_MECHANISM=SCRAM-SHA-512 \
        "$CONTAINER" rpk "$@"
}

echo "========================================="
echo " Waiting for the broker...                "
echo "========================================="

for _ in $(seq 1 30); do
    if rpk cluster health 2>/dev/null | grep -q "Healthy:.*true"; then
        echo "[OK] cluster healthy"
        break
    fi
    sleep 2
done

echo "========================================="
echo " Creating user '$APP_USER'...             "
echo "========================================="

rpk security user create "$APP_USER" \
    --password "$CORE_APP_PASSWORD" \
    --mechanism SCRAM-SHA-512 \
    || echo "[INFO] user already exists, continuing"

echo "========================================="
echo " Granting ACLs to '$APP_USER'...          "
echo "========================================="

# The app produces, consumes and creates its own topic through the admin
# client, so it needs the topic, its consumer group, and cluster-level create.
rpk security acl create \
    --allow-principal "User:$APP_USER" \
    --operation all \
    --topic "$APP_TOPIC"

rpk security acl create \
    --allow-principal "User:$APP_USER" \
    --operation all \
    --group "$APP_GROUP"

rpk security acl create \
    --allow-principal "User:$APP_USER" \
    --operation create,describe \
    --cluster

echo "========================================="
echo " Current users and ACLs                   "
echo "========================================="

rpk security user list
rpk security acl list

cat <<EOF

Done. Point the app at the host listener with these credentials:

    APP__KAFKA__BROKER=localhost:19092
    APP__KAFKA__SASL_USERNAME=$APP_USER
    APP__KAFKA__SASL_PASSWORD=<CORE_APP_PASSWORD>

To add an IoT or mobile device, give it its OWN user rather than sharing this
one -- a shared credential cannot be revoked without breaking the whole fleet.
Prefer produce-only where the device never consumes:

    rpk security user create <device> --password <pw> --mechanism SCRAM-SHA-512
    rpk security acl create --allow-principal User:<device> \\
        --operation write,describe --topic <its-topic>
EOF
