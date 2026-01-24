#!/bin/bash
VPS_IP="$1"
VPS_USER="${2:-pms}"

if [ -z "$VPS_IP" ]; then
    echo "Usage: ./check_caddy_logs.sh <VPS_IP> [USER]"
    echo "Example: ./check_caddy_logs.sh 87.106.50.82 pms"
    exit 1
fi

echo "🔍 Fetching Caddy logs from $VPS_USER@$VPS_IP..."
ssh -t $VPS_USER@$VPS_IP "cd /opt/pms && docker compose logs --tail 100 caddy"
