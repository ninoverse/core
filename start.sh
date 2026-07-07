#!/bin/bash

echo "[STEP 1] Compose down"
docker-compose down -v

echo "[STEP 2] Compose up"
docker-compose up -d

echo "[STEP 3] Wait 30sec for postgres"
sleep 30

echo "[STEP 4] Cargo run"
APP__PORT=7878  APP__HOST=localhost APP__DATABASE__HOST=localhost APP__DATABASE__PORT=5432 APP__DATABASE__USER=nino APP__DATABASE__PASSWORD=nino APP__DATABASE__DATABASE=ninoverse APP__DATABASE__POOL_SIZE=25 APP__KAFKA__BROKER=localhost:9092 APP__KAFKA__TOPICS=ninoverse:1:1 cargo run