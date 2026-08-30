#!/bin/bash
TOOLS_CONTAINER="kanidm-cli"
SERVER_CONTAINER="kanidmd"
ADMIN_NAME="idm_admin"

echo "========================================="
echo " Configuring Kanidm CLI container...     "
echo "========================================="

docker exec -it "$TOOLS_CONTAINER" sh -c '
mkdir -p /root/.config
echo "uri = \"https://kanidmd:8443\"" > /root/.config/kanidm
echo "verify_ca = false" >> /root/.config/kanidm
echo "verify_hostnames = false" >> /root/.config/kanidm
'

if [ $? -eq 0 ]; then
    echo "[OK] CLI configured to connect to https://kanidmd:8443"
else
    echo "[ERROR] Failed to configure CLI container."
    exit 1
fi

echo "========================================="
echo " Generating Initial Admin Password...    "
echo "========================================="

docker exec -it "$SERVER_CONTAINER" kanidmd recover-account -c /data/server.toml $ADMIN_NAME

echo "========================================="
echo " Login to CLI:                             "
echo "========================================="

docker exec -it $TOOLS_CONTAINER kanidm login --name $ADMIN_NAME

echo "========================================="
echo " Disable TOTM for dev environment:                             "
echo "========================================="

docker exec -it $TOOLS_CONTAINER kanidm group account-policy credential-type-minimum idm_all_persons any
