#!/bin/bash
# k3s runs outside the orchestrator: crates/core/src/docker.rs cannot express
# privileged / tmpfs / ulimits, without which k3s dies on the root cgroup.
# The orchestrator must be up first -- etcd-backend and ninoverse-network are
# joined as external here.
# Tear down with:
#   docker compose -f /home/nino/repositories/core/crates/core/containers/standalone/k3s.yaml down
set -eux
docker compose -f /home/nino/repositories/core/crates/core/containers/standalone/k3s.yaml up -d
