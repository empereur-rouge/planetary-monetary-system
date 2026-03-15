#!/bin/bash
# =============================================================================
# PMS Testnet Upgrade — Update code without touching data
# =============================================================================
# Usage: ./upgrade-testnet.sh <VPS_IP> [USER] [SSH_KEY]
#
# This script upgrades the testnet by rebuilding Docker images and restarting
# containers. It does NOT touch:
#   - RocksDB data (blockchain state, UTXO set)
#   - Coordinator wallet/keys
#   - Treasury wallets/keys
#   - API keys (sdk-api-key, api-keys.json)
#   - TLS certificates
#   - Node identity (node.key)
#   - Prometheus data
#   - Caddy certificates (Let's Encrypt)
#
# For a full fresh deployment, use deploy-testnet.sh instead.
# =============================================================================

set -e

VPS_IP="${1:-}"
VPS_USER="${2:-pms}"
SSH_KEY="${3:-}"

SSH_OPTS="-o ServerAliveInterval=30 -o ServerAliveCountMax=5 -o ConnectTimeout=10"
if [ -n "$SSH_KEY" ]; then
    SSH_OPTS="$SSH_OPTS -i $SSH_KEY"
fi

DOMAIN_NAME="testnet.pms-network.com"
COMPOSE_FILE="docker-compose.testnet.yml"
CONFIG_FILE="etc/config/config.testnet.toml"
REMOTE_DIR="/opt/pms"
LOCAL_IMG_DIR="/tmp/pms-images"

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

if [ -z "$VPS_IP" ]; then
    echo "Usage: $0 <VPS_IP> [USER] [SSH_KEY]"
    echo "Example: $0 87.106.50.82 pms ~/.ssh/pms_vps"
    exit 1
fi

echo -e "${CYAN}===============================================${NC}"
echo -e "${CYAN}   PMS Testnet Upgrade (Code Only)${NC}"
echo -e "${CYAN}===============================================${NC}"
echo "   Target: $VPS_USER@$VPS_IP"
echo "   Domain: $DOMAIN_NAME"
echo -e "   ${BOLD}Data will NOT be touched.${NC}"
echo ""

# -----------------------------------------------------------------------------
# Utilities
# -----------------------------------------------------------------------------
ask_yes_no() {
    local prompt="$1"
    local default="$2"
    local reply

    if [ "$default" = "Y" ]; then
        prompt="$prompt [Y/n]"
    else
        prompt="$prompt [y/N]"
    fi

    while true; do
        read -p "$prompt " reply
        if [ -z "$reply" ]; then
            reply=$default
        fi
        case "$reply" in
            Y|y) return 0 ;;
            N|n) return 1 ;;
            *) echo "Please answer Y or N." ;;
        esac
    done
}

# -----------------------------------------------------------------------------
# 1. Select what to upgrade
# -----------------------------------------------------------------------------
echo -e "${YELLOW}[1/5] Select components to upgrade${NC}"

BUILD_ENGINE=true
BUILD_GATEWAY=true
BUILD_SIMULATOR=true
UPDATE_CONFIG=true

if ask_yes_no "   Upgrade Engine (pms-node)?" "Y"; then
    BUILD_ENGINE=true
else
    BUILD_ENGINE=false
fi

if ask_yes_no "   Upgrade Gateway (pms-gateway)?" "Y"; then
    BUILD_GATEWAY=true
else
    BUILD_GATEWAY=false
fi

if ask_yes_no "   Upgrade Simulator (pms-simulator)?" "Y"; then
    BUILD_SIMULATOR=true
else
    BUILD_SIMULATOR=false
fi

if ask_yes_no "   Update config files (preserving keys/addresses)?" "Y"; then
    UPDATE_CONFIG=true
else
    UPDATE_CONFIG=false
fi

if [ "$BUILD_ENGINE" = "false" ] && [ "$BUILD_GATEWAY" = "false" ] && [ "$BUILD_SIMULATOR" = "false" ] && [ "$UPDATE_CONFIG" = "false" ]; then
    echo -e "   ${YELLOW}Nothing selected — exiting.${NC}"
    exit 0
fi

# -----------------------------------------------------------------------------
# 2. Build selected images locally (cross-compile linux/amd64)
# -----------------------------------------------------------------------------
NEED_BUILD=false
if [ "$BUILD_ENGINE" = "true" ] || [ "$BUILD_GATEWAY" = "true" ] || [ "$BUILD_SIMULATOR" = "true" ]; then
    NEED_BUILD=true
fi

if [ "$NEED_BUILD" = "true" ]; then
    echo ""
    echo -e "${YELLOW}[2/5] Building images locally (linux/amd64)...${NC}"

    mkdir -p "$LOCAL_IMG_DIR"

    # Setup buildx builder for cross-platform
    if ! docker buildx inspect pms-builder >/dev/null 2>&1; then
        echo -e "   Creating buildx builder..."
        docker buildx create --name pms-builder --use >/dev/null 2>&1
    else
        docker buildx use pms-builder >/dev/null 2>&1
    fi

    if [ "$BUILD_ENGINE" = "true" ]; then
        echo -e "   ${CYAN}Building Engine...${NC}"
        docker buildx build \
            --platform linux/amd64 \
            -t pms-node:testnet \
            --output "type=docker,dest=$LOCAL_IMG_DIR/pms-node.tar" \
            .
        echo -e "   ${GREEN}Engine built.${NC}"
    fi

    if [ "$BUILD_GATEWAY" = "true" ]; then
        echo -e "   ${CYAN}Building Gateway...${NC}"
        docker buildx build \
            --platform linux/amd64 \
            -t pms-gateway:testnet \
            -f Dockerfile.gateway \
            --output "type=docker,dest=$LOCAL_IMG_DIR/pms-gateway.tar" \
            .
        echo -e "   ${GREEN}Gateway built.${NC}"
    fi

    if [ "$BUILD_SIMULATOR" = "true" ]; then
        echo -e "   ${CYAN}Building Simulator...${NC}"
        docker buildx build \
            --platform linux/amd64 \
            -t pms-simulator:testnet \
            -f Dockerfile.simulator \
            --output "type=docker,dest=$LOCAL_IMG_DIR/pms-simulator.tar" \
            .
        echo -e "   ${GREEN}Simulator built.${NC}"
    fi

    # Compress
    echo -e "   Compressing images..."
    for f in "$LOCAL_IMG_DIR"/*.tar; do
        gzip -f "$f"
    done

    TOTAL_SIZE=$(du -sh "$LOCAL_IMG_DIR" | awk '{print $1}')
    echo -e "   ${GREEN}Images built and compressed. Total: $TOTAL_SIZE${NC}"
else
    echo ""
    echo -e "${YELLOW}[2/5] No images to build.${NC}"
fi

# -----------------------------------------------------------------------------
# 3. Transfer files to VPS
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}[3/5] Transferring files to VPS...${NC}"

# Upload config files (preserving VPS-specific values)
if [ "$UPDATE_CONFIG" = "true" ]; then
    echo -e "   Saving VPS config values before overwrite..."

    # Save all VPS-specific config values
    SAVED_COORD_KEY=$(ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
        "grep '^coordinator_public_key' $REMOTE_DIR/$CONFIG_FILE 2>/dev/null" || echo "")
    SAVED_X25519_KEY=$(ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
        "grep '^coordinator_x25519_public_key' $REMOTE_DIR/$CONFIG_FILE 2>/dev/null" || echo "")
    SAVED_WALLET_ADDRS=$(ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
        "grep '^wallet_addresses' $REMOTE_DIR/$CONFIG_FILE 2>/dev/null" || echo "")
    SAVED_TREASURY_ADDRS=$(ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
        "grep '^treasury_addresses' $REMOTE_DIR/$CONFIG_FILE 2>/dev/null" || echo "")
    SAVED_SIGNER_PUBKEYS=$(ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
        "grep '^signer_pubkeys' $REMOTE_DIR/$CONFIG_FILE 2>/dev/null" || echo "")

    echo -e "   ${GREEN}Saved: coordinator_key, x25519_key, wallet_addresses, treasury_addresses, signer_pubkeys${NC}"

    # Upload fresh config + compose + simulator config
    scp -q $SSH_OPTS "$COMPOSE_FILE" "$VPS_USER@$VPS_IP:$REMOTE_DIR/$COMPOSE_FILE"
    scp -q $SSH_OPTS "$CONFIG_FILE" "$VPS_USER@$VPS_IP:$REMOTE_DIR/$CONFIG_FILE"
    scp -q $SSH_OPTS tools/simulator/simulator.testnet.toml "$VPS_USER@$VPS_IP:$REMOTE_DIR/tools/simulator/simulator.testnet.toml"
    scp -q $SSH_OPTS tools/simulator/agents_testnet.toml "$VPS_USER@$VPS_IP:$REMOTE_DIR/tools/simulator/agents_testnet.toml"

    # Restore VPS-specific values.
    # IMPORTANT: Use heredocs (<< EOF) instead of ssh "..." for sed commands.
    # TOML values contain double quotes (e.g. key = "02abc..."). In ssh "...",
    # those inner " break shell quoting and get stripped → invalid TOML → engine crash.
    # In heredocs, " is always literal — no quoting conflict.
    echo -e "   Restoring VPS config values..."
    if [ -n "$SAVED_COORD_KEY" ]; then
        ssh -T -q $SSH_OPTS "$VPS_USER@$VPS_IP" << RESTORE_COORD_EOF
sed -i 's|^coordinator_public_key = .*|$SAVED_COORD_KEY|' $REMOTE_DIR/$CONFIG_FILE
RESTORE_COORD_EOF
    fi
    if [ -n "$SAVED_X25519_KEY" ]; then
        ssh -T -q $SSH_OPTS "$VPS_USER@$VPS_IP" << RESTORE_X25519_EOF
sed -i 's|^coordinator_x25519_public_key = .*|$SAVED_X25519_KEY|' $REMOTE_DIR/$CONFIG_FILE
RESTORE_X25519_EOF
    fi
    if [ -n "$SAVED_WALLET_ADDRS" ]; then
        ssh -T -q $SSH_OPTS "$VPS_USER@$VPS_IP" << RESTORE_WALLET_EOF
sed -i 's|^wallet_addresses = .*|$SAVED_WALLET_ADDRS|' $REMOTE_DIR/$CONFIG_FILE
RESTORE_WALLET_EOF
    fi
    if [ -n "$SAVED_TREASURY_ADDRS" ]; then
        ssh -T -q $SSH_OPTS "$VPS_USER@$VPS_IP" << RESTORE_TREASURY_EOF
if grep -q '^treasury_addresses' $REMOTE_DIR/$CONFIG_FILE; then
    sed -i 's|^treasury_addresses = .*|$SAVED_TREASURY_ADDRS|' $REMOTE_DIR/$CONFIG_FILE
else
    sed -i '/^\[fees\]/a $SAVED_TREASURY_ADDRS' $REMOTE_DIR/$CONFIG_FILE
fi
RESTORE_TREASURY_EOF
    fi
    if [ -n "$SAVED_SIGNER_PUBKEYS" ]; then
        ssh -T -q $SSH_OPTS "$VPS_USER@$VPS_IP" << RESTORE_SIGNER_EOF
if grep -q '^signer_pubkeys' $REMOTE_DIR/$CONFIG_FILE; then
    sed -i 's|^signer_pubkeys = .*|$SAVED_SIGNER_PUBKEYS|' $REMOTE_DIR/$CONFIG_FILE
else
    sed -i '/^\[admin\]/a $SAVED_SIGNER_PUBKEYS' $REMOTE_DIR/$CONFIG_FILE
fi
RESTORE_SIGNER_EOF
    fi
    echo -e "   ${GREEN}Config files uploaded and VPS values restored.${NC}"
fi

# Upload Docker images
if [ "$NEED_BUILD" = "true" ]; then
    echo -e "   Uploading Docker images to VPS..."
    ssh -T $SSH_OPTS "$VPS_USER@$VPS_IP" "mkdir -p /tmp/pms-images"

    for img in pms-node pms-gateway pms-simulator; do
        TARBALL="$LOCAL_IMG_DIR/$img.tar.gz"
        if [ -f "$TARBALL" ]; then
            SIZE=$(du -sh "$TARBALL" | awk '{print $1}')
            echo -e "   Uploading $img ($SIZE)..."
            scp $SSH_OPTS "$TARBALL" "$VPS_USER@$VPS_IP:/tmp/pms-images/"
        fi
    done

    echo -e "   ${GREEN}Images uploaded.${NC}"
    rm -rf "$LOCAL_IMG_DIR"
fi

# -----------------------------------------------------------------------------
# 4. Remote: Load images + Restart containers (preserve data)
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}[4/5] Upgrading on VPS (preserving all data)...${NC}"

# Retrieve the admin token from the running container (so we don't ask for it)
ADMIN_TOKEN=$(ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
    "docker inspect pms-engine-testnet --format='{{range .Config.Env}}{{println .}}{{end}}' 2>/dev/null | grep '^PMS_ADMIN_TOKEN=' | cut -d= -f2" || echo "")

if [ -z "$ADMIN_TOKEN" ]; then
    echo -e "   ${YELLOW}Could not retrieve admin token from running container.${NC}"
    read -p "   Enter ADMIN_TOKEN: " ADMIN_TOKEN
    if [ -z "$ADMIN_TOKEN" ]; then
        echo -e "   ${RED}Admin token required. Aborting.${NC}"
        exit 1
    fi
fi
echo -e "   ${GREEN}Admin token: ${ADMIN_TOKEN:0:8}...${NC}"

# Also retrieve PMS_API_KEY for the simulator
API_KEY=$(ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
    "docker inspect pms-simulator-testnet --format='{{range .Config.Env}}{{println .}}{{end}}' 2>/dev/null | grep '^PMS_API_KEY=' | cut -d= -f2" || echo "")

if [ -n "$API_KEY" ]; then
    echo -e "   ${GREEN}Simulator API key recovered: ${API_KEY:0:12}...${NC}"
fi

# Retrieve coordinator credentials for simulator
COORD_KEY=$(ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
    "python3 -c \"import json; print(json.load(open('$REMOTE_DIR/etc/pms/coordinator.json'))['private_key'])\" 2>/dev/null" || echo "")
COORD_ADDR=$(ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
    "python3 -c \"import json; print(json.load(open('$REMOTE_DIR/etc/pms/coordinator.json'))['address'])\" 2>/dev/null" || echo "")

if [ -n "$COORD_KEY" ] && [ -n "$COORD_ADDR" ]; then
    echo -e "   ${GREEN}Coordinator credentials recovered: ${COORD_ADDR:0:12}...${NC}"
else
    echo -e "   ${YELLOW}Could not retrieve coordinator credentials (coordinator agent won't start).${NC}"
fi

# Build list of services to recreate
SERVICES_TO_RECREATE=""
if [ "$BUILD_ENGINE" = "true" ]; then
    SERVICES_TO_RECREATE="$SERVICES_TO_RECREATE pms-engine"
fi
if [ "$BUILD_GATEWAY" = "true" ]; then
    SERVICES_TO_RECREATE="$SERVICES_TO_RECREATE pms-gateway"
fi
if [ "$BUILD_SIMULATOR" = "true" ]; then
    SERVICES_TO_RECREATE="$SERVICES_TO_RECREATE pms-simulator"
fi
# If config changed, restart everything
if [ "$UPDATE_CONFIG" = "true" ] && [ -z "$SERVICES_TO_RECREATE" ]; then
    SERVICES_TO_RECREATE="pms-engine pms-gateway pms-simulator"
fi

ssh -T $SSH_OPTS "$VPS_USER@$VPS_IP" << EOFREMOTE
set -e

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

cd $REMOTE_DIR

# --- Load new Docker images ---
if ls /tmp/pms-images/*.tar.gz 1>/dev/null 2>&1; then
    echo -e "\${YELLOW}   Loading new Docker images...\${NC}"
    for img in /tmp/pms-images/*.tar.gz; do
        NAME=\$(basename "\$img" .tar.gz)
        echo -n "   Loading \$NAME... "
        gunzip -c "\$img" | docker load 2>&1 | tail -1
    done
    rm -rf /tmp/pms-images
    echo -e "\${GREEN}   All images loaded.\${NC}"
fi

# --- Restart only the selected services (NO volume deletion) ---
echo ""
echo -e "\${YELLOW}   Restarting services:$SERVICES_TO_RECREATE\${NC}"
echo -e "   \${BOLD}Volumes and data will NOT be deleted.\${NC}"

export PMS_ADMIN_TOKEN="$ADMIN_TOKEN"
export PMS_API_KEY="$API_KEY"
export PMS_COORDINATOR_KEY="$COORD_KEY"
export PMS_COORDINATOR_ADDR="$COORD_ADDR"

docker compose -f $COMPOSE_FILE up -d --force-recreate $SERVICES_TO_RECREATE

# If we upgraded engine or gateway, caddy may need a restart too (depends_on)
if echo "$SERVICES_TO_RECREATE" | grep -q "pms-gateway"; then
    echo -e "\${YELLOW}   Restarting Caddy (depends on gateway)...\${NC}"
    docker compose -f $COMPOSE_FILE up -d --force-recreate caddy
fi

echo -e "\${GREEN}   Containers restarted.\${NC}"

# --- Health checks ---
echo ""
echo -e "\${YELLOW}   Waiting for Engine...\${NC}"
for i in \$(seq 1 30); do
    if docker exec pms-engine-testnet bash -c 'timeout 2 bash -c "</dev/tcp/127.0.0.1/8080"' 2>/dev/null; then
        echo -e "   \${GREEN}Engine is UP\${NC}"
        break
    fi
    if [ \$i -eq 30 ]; then
        echo -e "   \${RED}Engine failed to start!\${NC}"
        docker compose -f $COMPOSE_FILE logs --tail 30 pms-engine
        exit 1
    fi
    echo -n "."
    sleep 1
done

echo -e "\${YELLOW}   Waiting for Gateway...\${NC}"
for i in \$(seq 1 30); do
    if docker exec pms-gateway-testnet bash -c 'timeout 2 bash -c "</dev/tcp/127.0.0.1/8443"' 2>/dev/null; then
        echo -e "   \${GREEN}Gateway is UP\${NC}"
        break
    fi
    if [ \$i -eq 30 ]; then
        echo -e "   \${RED}Gateway failed to start!\${NC}"
        docker compose -f $COMPOSE_FILE logs --tail 30 pms-gateway
        exit 1
    fi
    echo -n "."
    sleep 1
done

echo -e "\${YELLOW}   Waiting for Simulator...\${NC}"
for i in \$(seq 1 15); do
    if curl -sf http://127.0.0.1:9090/ > /dev/null 2>&1; then
        echo -e "   \${GREEN}Simulator is UP\${NC}"
        break
    fi
    if [ \$i -eq 15 ]; then
        echo -e "   \${YELLOW}Simulator not responding (check: docker logs pms-simulator-testnet)\${NC}"
    fi
    echo -n "."
    sleep 2
done
echo ""

# Prune old images
echo -e "\${YELLOW}   Pruning old Docker images...\${NC}"
docker image prune -f 2>/dev/null || true

# Final status
echo ""
echo -e "\${YELLOW}   Service Status:\${NC}"
docker compose -f $COMPOSE_FILE ps

EOFREMOTE

# -----------------------------------------------------------------------------
# 5. Connectivity check
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}[5/5] Connectivity check...${NC}"

echo "   Testing: https://$DOMAIN_NAME/livez"
if HTTP_STATUS=$(curl -sk -o /dev/null -w "%{http_code}" "https://$DOMAIN_NAME/livez" 2>/dev/null); then
    if [ "$HTTP_STATUS" == "200" ]; then
        echo -e "   ${GREEN}$HTTP_STATUS OK — Testnet is live!${NC}"
    else
        echo -e "   ${YELLOW}$HTTP_STATUS — Server reachable but returned error.${NC}"
    fi
else
    echo -e "   ${RED}Connection failed (DNS propagating or Caddy restarting).${NC}"
fi

# Quick data integrity check
echo ""
echo -e "${YELLOW}   Data integrity check...${NC}"
BLOCK_COUNT=$(curl -sk "https://$DOMAIN_NAME/stats" 2>/dev/null | python3 -c "import json,sys; print(json.load(sys.stdin).get('block_count', '?'))" 2>/dev/null || echo "?")
UTXO_COUNT=$(curl -sk "https://$DOMAIN_NAME/stats" 2>/dev/null | python3 -c "import json,sys; print(json.load(sys.stdin).get('utxo_count', '?'))" 2>/dev/null || echo "?")
echo -e "   Blocks: ${GREEN}$BLOCK_COUNT${NC}"
echo -e "   UTXOs:  ${GREEN}$UTXO_COUNT${NC}"

echo ""
echo -e "${CYAN}===============================================${NC}"
echo -e "${CYAN}   PMS Testnet Upgrade Complete${NC}"
echo -e "${CYAN}===============================================${NC}"
echo ""
echo "   Services:"
echo "   - Testnet API:     https://$DOMAIN_NAME"
echo "   - Dashboard:       https://$DOMAIN_NAME/dashboard/"
echo "   - Simulator:       http://$VPS_IP:9090"
echo ""
echo "   Useful commands (on VPS):"
echo "   - Logs engine:     docker logs -f pms-engine-testnet"
echo "   - Logs gateway:    docker logs -f pms-gateway-testnet"
echo "   - Logs simulator:  docker logs -f pms-simulator-testnet"
echo "   - Status:          docker compose -f $COMPOSE_FILE ps"
echo ""
