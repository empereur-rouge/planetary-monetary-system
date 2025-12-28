#!/bin/bash
set -e

echo "🔥 1. Starting fresh..."
docker compose up -d node caddy
# Wait for health
echo "⏳ Waiting for node to be healthy..."
sleep 5
until curl -k -sfm1 https://127.0.0.1:8080/livez; do echo "."; sleep 1; done
echo ""

echo "🧱 2. Injecting data (E2E Test)..."
cargo test --package pms-server --test docker_e2e -- --ignored --nocapture

echo "📊 Counting blocks (Before Crash)..."
# We fetch metrics to see pms_blocks_total. grep for it.
curl -k -s -H "Authorization: Bearer pms_admin_secret" https://127.0.0.1:8080/metrics | grep "pms_blocks_total" || echo "Metric not found yet"

echo "☠️ 3. KILLING NODE (Simulating Crash)..."
docker compose kill node

echo "😴 Sleeping 5s..."
sleep 5

echo "🚑 4. Restarting Node..."
docker compose start node

echo "⏳ Waiting for node recovery..."
sleep 5
until curl -k -sfm1 https://127.0.0.1:8080/livez; do echo "."; sleep 1; done
echo "✅ Node is back online!"

echo "📊 Counting blocks (After Crash)..."
curl -k -s -H "Authorization: Bearer pms_admin_secret" https://127.0.0.1:8080/metrics | grep "pms_blocks_total"

echo "🧱 5. Verifying Write Capability (E2E Test again)..."
cargo test --package pms-server --test docker_e2e -- --ignored --nocapture

echo "🎉 Crash Recovery Test PASSED!"
