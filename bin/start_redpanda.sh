#!/bin/bash
# Redpanda runs outside the orchestrator: crates/core/src/docker.rs deserializes
# `command` as a String and does not interpolate ${VAR}, neither of which this
# definition can do without. The orchestrator must be up first --
# ninoverse-network is joined as external here.
#
# RP_ADMIN_PASSWORD must be set: it creates the superuser on an empty volume and
# is substituted into the config for the in-process proxy clients. Export it, or
# put it in an .env file beside the compose file.
#
# Tear down with:
#   docker compose -f /home/nino/repositories/core/crates/core/containers/standalone/redpanda.yaml down
set -eux

# ninoverse-network is normally created by the orchestrator, but on a fresh
# clone the orchestrator cannot get that far: init_kafka fails when no broker
# answers and the binary exits, tearing the stack down. Creating the network
# here breaks that deadlock -- create_docker_networks skips a network that
# already exists (it swallows the 409), so core is happy either way.
docker network inspect ninoverse-network >/dev/null 2>&1 ||
    docker network create ninoverse-network

docker compose -f /home/nino/repositories/core/crates/core/containers/standalone/redpanda.yaml up -d
