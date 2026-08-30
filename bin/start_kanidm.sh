#!/bin/bash
set -eux
cleanup() {
    docker compose -f /home/nino/repositories/core/crates/core/containers/kanidm.yaml down --remove-orphans
}
trap cleanup EXIT
docker compose -f /home/nino/repositories/core/crates/core/containers/kanidm.yaml up
