#!/bin/bash
# =============================================================================
# PMS Mainnet Upgrade — Update code without touching data
# =============================================================================
# Usage: ./upgrade-mainnet.sh <VPS_IP> [USER] [SSH_KEY]
#        IMAGE_VERSION=v0.7.22 ./upgrade-mainnet.sh <VPS_IP> pms ~/.ssh/pms_vps
#
# Env vars:
#   IMAGE_VERSION    Tag semver pour les nouvelles images (défaut: v0.7.21).
#                    Bump ce tag à chaque release. Pour rollback :
#                    IMAGE_VERSION=v0.7.20 ./upgrade-mainnet.sh ...
#
# Ce script upgrade le mainnet en rebuildant les images Docker avec un
# nouveau tag semver et en restartant les containers. Il NE TOUCHE PAS :
#   - RocksDB data (blockchain state, UTXO set)
#   - Coordinator wallet/keys
#   - Treasury wallets/keys
#   - API keys (sdk-api-key, api-keys.json)
#   - TLS certificates
#   - Node identity (node.key)
#   - Prometheus data + Alertmanager data
#   - Caddy certificates (Let's Encrypt)
#
# Pour un fresh deploy complet, utiliser deploy-mainnet.sh.
# =============================================================================

set -e

VPS_IP="${1:-}"
VPS_USER="${2:-pms}"
SSH_KEY="${3:-}"

SSH_OPTS="-o ServerAliveInterval=30 -o ServerAliveCountMax=5 -o ConnectTimeout=10"
if [ -n "$SSH_KEY" ]; then
    SSH_OPTS="$SSH_OPTS -i $SSH_KEY"
fi

IMAGE_VERSION="${IMAGE_VERSION:-v0.7.21}"

DOMAIN_NAME="pms-network.com"
COMPOSE_FILE="docker-compose.mainnet.yml"
CONFIG_FILE="etc/config/config.mainnet.toml"
REMOTE_DIR="/opt/pms"
LOCAL_IMG_DIR="/tmp/pms-images-mainnet"

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

if [ -z "$VPS_IP" ]; then
    echo "Usage: $0 <VPS_IP> [USER] [SSH_KEY]"
    echo "       IMAGE_VERSION=v0.7.22 $0 87.106.x.y pms ~/.ssh/pms_vps"
    echo ""
    echo "Env IMAGE_VERSION = tag semver Docker (défaut: v0.7.21)"
    exit 1
fi

echo -e "${CYAN}===============================================${NC}"
echo -e "${CYAN}   PMS Mainnet Upgrade (Code Only)${NC}"
echo -e "${CYAN}===============================================${NC}"
echo "   Target:        $VPS_USER@$VPS_IP"
echo "   Domain:        $DOMAIN_NAME"
echo "   Image version: $IMAGE_VERSION"
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
# 1. Component selection
# -----------------------------------------------------------------------------
echo -e "${YELLOW}[1/5] Select components to upgrade${NC}"

BUILD_ENGINE=true
BUILD_GATEWAY=true
UPDATE_CONFIG=true

if ask_yes_no "   Upgrade Engine (pms-node:$IMAGE_VERSION)?" "Y"; then
    BUILD_ENGINE=true
else
    BUILD_ENGINE=false
fi

if ask_yes_no "   Upgrade Gateway (pms-gateway:$IMAGE_VERSION)?" "Y"; then
    BUILD_GATEWAY=true
else
    BUILD_GATEWAY=false
fi

if ask_yes_no "   Update config files (preserving keys/addresses)?" "Y"; then
    UPDATE_CONFIG=true
else
    UPDATE_CONFIG=false
fi

if [ "$BUILD_ENGINE" = "false" ] && [ "$BUILD_GATEWAY" = "false" ] && [ "$UPDATE_CONFIG" = "false" ]; then
    echo -e "   ${YELLOW}Nothing selected — exiting.${NC}"
    exit 0
fi

# -----------------------------------------------------------------------------
# 2. Build selected images
# -----------------------------------------------------------------------------
NEED_BUILD=false
if [ "$BUILD_ENGINE" = "true" ] || [ "$BUILD_GATEWAY" = "true" ]; then
    NEED_BUILD=true
fi

if [ "$NEED_BUILD" = "true" ]; then
    echo ""
    echo -e "${YELLOW}[2/5] Building images locally (linux/amd64)...${NC}"
    echo -e "   Tag: ${BOLD}$IMAGE_VERSION${NC}"

    mkdir -p "$LOCAL_IMG_DIR"

    if ! docker buildx inspect pms-builder >/dev/null 2>&1; then
        echo -e "   Creating buildx builder..."
        docker buildx create --name pms-builder --use >/dev/null 2>&1
    else
        docker buildx use pms-builder >/dev/null 2>&1
    fi

    if [ "$BUILD_ENGINE" = "true" ]; then
        echo -e "   ${CYAN}Building Engine (pms-node:$IMAGE_VERSION)...${NC}"
        docker buildx build \
            --platform linux/amd64 \
            -t "pms-node:$IMAGE_VERSION" \
            --output "type=docker,dest=$LOCAL_IMG_DIR/pms-node.tar" \
            .
        echo -e "   ${GREEN}Engine built.${NC}"
    fi

    if [ "$BUILD_GATEWAY" = "true" ]; then
        echo -e "   ${CYAN}Building Gateway (pms-gateway:$IMAGE_VERSION)...${NC}"
        docker buildx build \
            --platform linux/amd64 \
            -t "pms-gateway:$IMAGE_VERSION" \
            -f Dockerfile.gateway \
            --output "type=docker,dest=$LOCAL_IMG_DIR/pms-gateway.tar" \
            .
        echo -e "   ${GREEN}Gateway built.${NC}"
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

if [ "$UPDATE_CONFIG" = "true" ]; then
    echo -e "   Saving VPS config values before overwrite..."

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

    scp -q $SSH_OPTS "$COMPOSE_FILE" "$VPS_USER@$VPS_IP:$REMOTE_DIR/$COMPOSE_FILE"
    scp -q $SSH_OPTS "$CONFIG_FILE" "$VPS_USER@$VPS_IP:$REMOTE_DIR/$CONFIG_FILE"
    scp -q $SSH_OPTS etc/prometheus/prometheus.mainnet.yml "$VPS_USER@$VPS_IP:$REMOTE_DIR/etc/prometheus/prometheus.mainnet.yml"
    scp -q $SSH_OPTS etc/prometheus/alerting_rules.yml "$VPS_USER@$VPS_IP:$REMOTE_DIR/etc/prometheus/alerting_rules.yml"
    ssh -T -q $SSH_OPTS "$VPS_USER@$VPS_IP" "mkdir -p $REMOTE_DIR/etc/alertmanager"
    scp -q $SSH_OPTS etc/alertmanager/alertmanager.mainnet.yml "$VPS_USER@$VPS_IP:$REMOTE_DIR/etc/alertmanager/alertmanager.mainnet.yml"

    # Boot-resiliency: compose.yaml symlink → mainnet, legacy disable
    ssh -T -q $SSH_OPTS "$VPS_USER@$VPS_IP" "cd $REMOTE_DIR && \
        ln -sf docker-compose.mainnet.yml compose.yaml && \
        if [ -f docker-compose.yml ] && [ ! -L docker-compose.yml ]; then \
            mv docker-compose.yml docker-compose.legacy-prod.yml.disabled; \
            echo '   Disabled legacy docker-compose.yml'; \
        fi"

    # Restore VPS-specific values via heredocs
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
    ssh -T $SSH_OPTS "$VPS_USER@$VPS_IP" "mkdir -p /tmp/pms-images-mainnet"

    for img in pms-node pms-gateway; do
        TARBALL="$LOCAL_IMG_DIR/$img.tar.gz"
        if [ -f "$TARBALL" ]; then
            SIZE=$(du -sh "$TARBALL" | awk '{print $1}')
            echo -e "   Uploading $img ($SIZE)..."
            scp $SSH_OPTS "$TARBALL" "$VPS_USER@$VPS_IP:/tmp/pms-images-mainnet/"
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

# Retrieve admin token from running container
ADMIN_TOKEN=$(ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
    "docker inspect pms-engine-mainnet --format='{{range .Config.Env}}{{println .}}{{end}}' 2>/dev/null | grep '^PMS_ADMIN_TOKEN=' | cut -d= -f2" || echo "")

if [ -z "$ADMIN_TOKEN" ]; then
    echo -e "   ${YELLOW}Could not retrieve admin token from running container.${NC}"
    read -p "   Enter ADMIN_TOKEN: " ADMIN_TOKEN
    if [ -z "$ADMIN_TOKEN" ]; then
        echo -e "   ${RED}Admin token required. Aborting.${NC}"
        exit 1
    fi
fi
echo -e "   ${GREEN}Admin token: ${ADMIN_TOKEN:0:8}...${NC}"

# Build list of services to recreate
SERVICES_TO_RECREATE=""
if [ "$BUILD_ENGINE" = "true" ]; then
    SERVICES_TO_RECREATE="$SERVICES_TO_RECREATE pms-engine"
fi
if [ "$BUILD_GATEWAY" = "true" ]; then
    SERVICES_TO_RECREATE="$SERVICES_TO_RECREATE pms-gateway"
fi
# If config changed, restart engine + gateway (no simulator in mainnet)
if [ "$UPDATE_CONFIG" = "true" ] && [ -z "$SERVICES_TO_RECREATE" ]; then
    SERVICES_TO_RECREATE="pms-engine pms-gateway"
fi

ssh -T $SSH_OPTS "$VPS_USER@$VPS_IP" << EOFREMOTE
set -e

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

cd $REMOTE_DIR
export IMAGE_VERSION="$IMAGE_VERSION"

# --- Load new Docker images ---
if ls /tmp/pms-images-mainnet/*.tar.gz 1>/dev/null 2>&1; then
    echo -e "\${YELLOW}   Loading new Docker images...\${NC}"
    for img in /tmp/pms-images-mainnet/*.tar.gz; do
        NAME=\$(basename "\$img" .tar.gz)
        echo -n "   Loading \$NAME... "
        gunzip -c "\$img" | docker load 2>&1 | tail -1
    done
    rm -rf /tmp/pms-images-mainnet
    echo -e "\${GREEN}   All images loaded.\${NC}"
fi

# --- Restart only the selected services (NO volume deletion) ---
echo ""
echo -e "\${YELLOW}   Restarting services:$SERVICES_TO_RECREATE\${NC}"
echo -e "   \${BOLD}Volumes and data will NOT be deleted.\${NC}"

export PMS_ADMIN_TOKEN="$ADMIN_TOKEN"

# Refresh prometheus admin-token file
mkdir -p secrets
printf '%s' "$ADMIN_TOKEN" > secrets/prometheus_admin_token
chmod 644 secrets/prometheus_admin_token

# Telegram bot token placeholder if absent
if [ ! -s secrets/telegram_bot_token ]; then
    printf '%s' 'PLACEHOLDER_PASTE_BOT_TOKEN_FROM_BOTFATHER_HERE' > secrets/telegram_bot_token
fi
chmod 644 secrets/telegram_bot_token

# Targeted cleanup: stop+rm only services we're about to recreate.
docker compose -f $COMPOSE_FILE rm -f -s $SERVICES_TO_RECREATE 2>/dev/null || true
for _svc_to_rm in $SERVICES_TO_RECREATE; do
    case "\$_svc_to_rm" in
        pms-engine)    _cname_rm="pms-engine-mainnet" ;;
        pms-gateway)   _cname_rm="pms-gateway-mainnet" ;;
        caddy)         _cname_rm="pms-caddy-mainnet" ;;
        prometheus)    _cname_rm="pms-prometheus-mainnet" ;;
        alertmanager)  _cname_rm="pms-alertmanager-mainnet" ;;
        cadvisor)      _cname_rm="pms-cadvisor-mainnet" ;;
        *) _cname_rm="" ;;
    esac
    [ -n "\$_cname_rm" ] && docker rm -f "\$_cname_rm" 2>/dev/null || true
done

# Pre-deploy guard: nuke any stale containers from a non-mainnet
# `docker-compose.yml` deploy lingering on the host (same trap as
# testnet — see CLAUDE.md "Containers fantômes non-testnet au reboot").
for _stale in pms-engine pms-gateway pms-caddy pms-prometheus pms-alertmanager pms-cadvisor; do
    if docker inspect "\$_stale" >/dev/null 2>&1; then
        echo -e "\${YELLOW}   Removing stale non-mainnet container: \$_stale\${NC}"
        docker rm -f "\$_stale" 2>/dev/null || true
    fi
done

# Start services. NO --remove-orphans (would wipe alertmanager/cadvisor/prometheus
# when SERVICES_TO_RECREATE is a subset).
docker compose -f $COMPOSE_FILE up -d --force-recreate $SERVICES_TO_RECREATE 2>&1 || true

# Verify each requested service is running
for _compose_svc in $SERVICES_TO_RECREATE; do
    case \$_compose_svc in
        pms-engine)    _cname="pms-engine-mainnet" ;;
        pms-gateway)   _cname="pms-gateway-mainnet" ;;
        caddy)         _cname="pms-caddy-mainnet" ;;
        prometheus)    _cname="pms-prometheus-mainnet" ;;
        alertmanager)  _cname="pms-alertmanager-mainnet" ;;
        cadvisor)      _cname="pms-cadvisor-mainnet" ;;
        *) _cname="" ;;
    esac
    if [ -n "\$_cname" ] && ! docker ps --filter "name=\$_cname" --filter "status=running" -q 2>/dev/null | grep -q .; then
        echo -e "\${YELLOW}   \$_cname not running — retrying individually...\${NC}"
        docker compose -f $COMPOSE_FILE up -d "\$_compose_svc" 2>/dev/null || true
        sleep 2
    fi
done

# If we upgraded gateway, caddy may need a restart too (depends_on)
if echo "$SERVICES_TO_RECREATE" | grep -q "pms-gateway"; then
    echo -e "\${YELLOW}   Restarting Caddy (depends on gateway)...\${NC}"
    docker compose -f $COMPOSE_FILE up -d --force-recreate caddy 2>&1 || true
fi

echo -e "\${GREEN}   Containers restarted.\${NC}"

# --- Health checks ---
echo ""
echo -e "\${YELLOW}   Waiting for Engine...\${NC}"
for i in \$(seq 1 30); do
    if docker exec pms-engine-mainnet bash -c 'timeout 2 bash -c "</dev/tcp/127.0.0.1/8080"' 2>/dev/null; then
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
    if docker exec pms-gateway-mainnet bash -c 'timeout 2 bash -c "</dev/tcp/127.0.0.1/8443"' 2>/dev/null; then
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
        echo -e "   ${GREEN}$HTTP_STATUS OK — Mainnet is live!${NC}"
    else
        echo -e "   ${YELLOW}$HTTP_STATUS — Server reachable but returned error.${NC}"
    fi
else
    echo -e "   ${RED}Connection failed (DNS propagating or Caddy restarting).${NC}"
fi

# Quick data integrity check
echo ""
echo -e "${YELLOW}   Data integrity check...${NC}"
BLOCK_COUNT=$(curl -sk "https://$DOMAIN_NAME/healthz" 2>/dev/null | python3 -c "import json,sys; print(json.load(sys.stdin).get('block_count', '?'))" 2>/dev/null || echo "?")
echo -e "   Blocks: ${GREEN}$BLOCK_COUNT${NC}"

echo ""
echo -e "${CYAN}===============================================${NC}"
echo -e "${CYAN}   PMS Mainnet Upgrade Complete${NC}"
echo -e "${CYAN}===============================================${NC}"
echo ""
echo "   Image version: $IMAGE_VERSION"
echo ""
echo "   Services:"
echo "   - Mainnet API:     https://$DOMAIN_NAME"
echo "   - Dashboard:       https://$DOMAIN_NAME/dashboard/"
echo ""
echo "   Useful commands (on VPS):"
echo "   - Logs engine:      docker logs -f pms-engine-mainnet"
echo "   - Logs gateway:     docker logs -f pms-gateway-mainnet"
echo "   - Status:           docker compose -f $COMPOSE_FILE ps"
echo ""
