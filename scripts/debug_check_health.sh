#!/bin/bash
set -e

VPS_IP="${1:-}"
VPS_USER="pms"

if [ -z "$VPS_IP" ]; then
    echo "Usage: ./debug_check_health.sh <VPS_IP>"
    exit 1
fi

echo "🔌 Connecting to $VPS_IP to debug..."

ssh -T $VPS_USER@$VPS_IP << EOFREMOTE
    cd /opt/pms
    
    echo "🐳 Stopping containers..."
    docker compose down 2>/dev/null

    echo "🐳 Starting node explicitly..."
    docker compose up -d node1

    echo "⏳ Waiting 10s for startup..."
    sleep 10

    echo "🔍 Executing curl from INSIDE the container..."
    docker exec pms-node curl -v -k https://127.0.0.1:8080/livez || echo "❌ CURL FAILED with exit code \$?"

    echo ""
    echo "🔍 Container logs (last 20 lines):"
    docker logs --tail 20 pms-node
EOFREMOTE
