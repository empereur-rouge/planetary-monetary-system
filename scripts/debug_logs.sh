#!/bin/bash
set -e

VPS_IP="${1:-}"
VPS_USER="pms"

if [ -z "$VPS_IP" ]; then
    echo "Usage: ./scripts/debug_logs.sh <VPS_IP>"
    exit 1
fi

echo "🔌 Connecting to $VPS_IP to fetch HEAD logs..."

ssh -T $VPS_USER@$VPS_IP << EOFREMOTE
    echo "🔍 Container logs (First 100 lines):"
    docker logs pms-node 2>&1 | head -n 100
EOFREMOTE
