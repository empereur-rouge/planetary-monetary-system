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

    # Upload fresh config + compose + simulator config + prometheus
    # scrape config (the v0.7.5 update flips /metrics → /metrics/all and
    # adds bearer-token auth, otherwise the new per-stage persist counters
    # never make it into Prometheus).
    scp -q $SSH_OPTS "$COMPOSE_FILE" "$VPS_USER@$VPS_IP:$REMOTE_DIR/$COMPOSE_FILE"
    scp -q $SSH_OPTS "$CONFIG_FILE" "$VPS_USER@$VPS_IP:$REMOTE_DIR/$CONFIG_FILE"
    scp -q $SSH_OPTS etc/prometheus/prometheus.yml "$VPS_USER@$VPS_IP:$REMOTE_DIR/etc/prometheus/prometheus.yml"
    scp -q $SSH_OPTS etc/prometheus/alerting_rules.yml "$VPS_USER@$VPS_IP:$REMOTE_DIR/etc/prometheus/alerting_rules.yml"
    # Alertmanager config — placeholder webhook URLs are safe to ship as
    # a default; alerts get silently dropped until the operator wires
    # real Better Uptime / PagerDuty / Slack URLs in
    # `etc/alertmanager/alertmanager.yml`.
    ssh -T -q $SSH_OPTS "$VPS_USER@$VPS_IP" "mkdir -p $REMOTE_DIR/etc/alertmanager"
    scp -q $SSH_OPTS etc/alertmanager/alertmanager.yml "$VPS_USER@$VPS_IP:$REMOTE_DIR/etc/alertmanager/alertmanager.yml"
    scp -q $SSH_OPTS tools/simulator/simulator.testnet.toml "$VPS_USER@$VPS_IP:$REMOTE_DIR/tools/simulator/simulator.testnet.toml"
    scp -q $SSH_OPTS tools/simulator/agents_testnet.toml "$VPS_USER@$VPS_IP:$REMOTE_DIR/tools/simulator/agents_testnet.toml"

    # Caddyfile (v0.30.2 XFF overwrite) — l'upgrade DOIT régénérer+scp le Caddyfile
    # comme deploy-testnet.sh, sinon il livre le gateway (qui forwarde XFF) SANS
    # l'écrasement Caddy `header_up X-Forwarded-For {remote_host}` → rate limiting
    # per-client spoofable/empoisonnable (revue sécu DoS v0.30.2). Bloc IDENTIQUE à
    # scripts/deploy-testnet.sh (garde `deploy_scripts_overwrite_xff` couvre les deux).
    cat > Caddyfile.testnet <<'CADDY_EOF'
{
    email admin@pms-network.com
}

testnet.pms-network.com {
    tls admin@pms-network.com
    reverse_proxy https://pms-gateway:8443 {
        # SECURITE (v0.30.2) : ECRASE X-Forwarded-For avec l'IP reelle du peer.
        header_up X-Forwarded-For {remote_host}
        transport http {
            tls
            tls_insecure_skip_verify
        }
    }
}
CADDY_EOF
    scp -q $SSH_OPTS Caddyfile.testnet "$VPS_USER@$VPS_IP:$REMOTE_DIR/Caddyfile.testnet"
    # Recharge Caddy en gracieux pour appliquer le Caddyfile même si le gateway
    # n'est pas recréé dans cet upgrade (le restart caddy plus bas ne se déclenche
    # que si pms-gateway est dans $SERVICES_TO_RECREATE).
    ssh -T -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
        "docker exec pms-caddy-testnet caddy reload --config /etc/caddy/Caddyfile 2>/dev/null || true"

    # Boot-resiliency: ensure `docker compose` (without `-f`) on the VPS
    # always picks up the testnet stack. Two invariants:
    #
    #   1. `compose.yaml` symlink → `docker-compose.testnet.yml` —
    #      Compose v2 prefers `compose.yaml` over `docker-compose.yml`,
    #      so plain `docker compose up -d` resolves to the testnet
    #      services. No more `-f` required, no more accidental legacy
    #      stack creation.
    #
    #   2. The legacy `docker-compose.yml` (the pre-multi-ledger prod
    #      compose) is renamed to `.disabled` if still present. Its
    #      service names collided with the testnet stack on `project=pms`
    #      label and bit us 2026-04-28 after a host reboot — see
    #      CLAUDE.md "Containers fantômes non-testnet au reboot".
    #
    # Both operations are idempotent — re-running the upgrade is safe.
    ssh -T -q $SSH_OPTS "$VPS_USER@$VPS_IP" "cd $REMOTE_DIR && \
        ln -sf docker-compose.testnet.yml compose.yaml && \
        if [ -f docker-compose.yml ] && [ ! -L docker-compose.yml ]; then \
            mv docker-compose.yml docker-compose.legacy-prod.yml.disabled; \
            echo '   Disabled legacy docker-compose.yml'; \
        fi"

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

# Refresh the Prometheus admin-token file every upgrade. /metrics/all
# sits behind require_local_or_admin and Prometheus runs in its own
# container (non-loopback), so the scrape config reads the token from
# /etc/prometheus/admin_token (mounted from secrets/prometheus_admin_token).
# This keeps the file in sync if the operator rotates the token.
mkdir -p secrets
printf '%s' "$ADMIN_TOKEN" > secrets/prometheus_admin_token
# 644 (not 600): prometheus container runs as `nobody` (uid 65534)
# while the file is owned by `pms` — 600 makes it unreadable to
# the container and the scrape errors out with
# "unable to read authorization credentials file". See deploy script
# comment for the full rationale.
chmod 644 secrets/prometheus_admin_token

# Telegram bot token for Alertmanager. The container expects a
# readable file at /etc/alertmanager/telegram_bot_token; if the
# operator hasn't created secrets/telegram_bot_token, write a
# placeholder so the bind-mount succeeds and alertmanager starts
# clean. Until the placeholder is replaced with a real token from
# @BotFather + chat_id is set in alertmanager.yml, alerts route to
# the `null` receiver (silently dropped) — see
# documentation/runbooks/alerting.md.
if [ ! -s secrets/telegram_bot_token ]; then
    printf '%s' 'PLACEHOLDER_PASTE_BOT_TOKEN_FROM_BOTFATHER_HERE' > secrets/telegram_bot_token
fi
chmod 644 secrets/telegram_bot_token

# Clean stale Docker Compose state (ghost container fix).
# Docker Compose v2 can desync with containerd, leaving phantom container
# references that cause "No such container" errors on recreate.
#
# Targeted cleanup: stop+rm only the services we're about to recreate.
# An earlier version of this block did `docker rm -f` on every pms-project
# container, which silently wiped pms-prometheus-testnet at every upgrade
# (it's not in the recreate list, so nothing brought it back). Keep the
# blast radius scoped to $SERVICES_TO_RECREATE.
docker compose -f $COMPOSE_FILE rm -f -s $SERVICES_TO_RECREATE 2>/dev/null || true
for _svc_to_rm in $SERVICES_TO_RECREATE; do
    case "\$_svc_to_rm" in
        pms-engine)    _cname_rm="pms-engine-testnet" ;;
        pms-gateway)   _cname_rm="pms-gateway-testnet" ;;
        pms-simulator) _cname_rm="pms-simulator-testnet" ;;
        caddy)         _cname_rm="pms-caddy-testnet" ;;
        prometheus)    _cname_rm="pms-prometheus-testnet" ;;
        *) _cname_rm="" ;;
    esac
    [ -n "\$_cname_rm" ] && docker rm -f "\$_cname_rm" 2>/dev/null || true
done

# Pre-deploy guard: nuke any stale containers from a non-testnet
# `docker-compose.yml` deploy that may still be lingering on the host.
# These have the same SERVICE name (pms-engine / pms-gateway / etc.)
# but NO `-testnet` suffix on their container_name, and are tied to
# `:latest` images instead of `:testnet`. They get auto-restarted by
# their own `restart: unless-stopped` policy after a host reboot,
# which then conflicts with our testnet stack on Docker DNS / network
# resolution (a clicker can suddenly find itself talking to pms-gateway
# on a stale `:latest` image instead of pms-gateway-testnet). Seen
# 2026-04-28 after the IONOS reboot — simulator was healthy but
# crash-looping with "Cannot reach gateway" because it was trying
# pms-testnet-public DNS while the live gateway was on pms-public.
# Removing them here is safe: their volumes (`rocksdb_data`, etc.) are
# named and persistent so the data survives; we just disconnect the
# container shell.
for _stale in pms-engine pms-gateway pms-caddy pms-prometheus pms-alertmanager; do
    if docker inspect "\$_stale" >/dev/null 2>&1; then
        echo -e "\${YELLOW}   Removing stale non-testnet container: \$_stale\${NC}"
        docker rm -f "\$_stale" 2>/dev/null || true
    fi
done

# Start services. Use || true because ghost containers may cause a non-zero
# exit even though the real services are created successfully.
#
# NOTE: NO --remove-orphans here. With $SERVICES_TO_RECREATE being a subset
# of the compose file, --remove-orphans was previously misclassifying
# pms-prometheus-testnet as orphaned and removing it on every upgrade
# (the script was thinking "you only asked for engine+gateway+simulator,
# so prometheus must be an orphan"). It's not — it's a sibling service
# we're choosing not to touch. Same fix on the caddy block below.
docker compose -f $COMPOSE_FILE up -d --force-recreate $SERVICES_TO_RECREATE 2>&1 || true

# Verify each requested service is running (retry individually if ghost blocked it)
for _compose_svc in $SERVICES_TO_RECREATE; do
    case \$_compose_svc in
        pms-engine)    _cname="pms-engine-testnet" ;;
        pms-gateway)   _cname="pms-gateway-testnet" ;;
        pms-simulator) _cname="pms-simulator-testnet" ;;
        caddy)         _cname="pms-caddy-testnet" ;;
        prometheus)    _cname="pms-prometheus-testnet" ;;
        *) _cname="" ;;
    esac
    if [ -n "\$_cname" ] && ! docker ps --filter "name=\$_cname" --filter "status=running" -q 2>/dev/null | grep -q .; then
        echo -e "\${YELLOW}   \$_cname not running — retrying individually...\${NC}"
        docker compose -f $COMPOSE_FILE up -d "\$_compose_svc" 2>/dev/null || true
        sleep 2
    fi
done

# If we upgraded engine or gateway, caddy may need a restart too (depends_on)
if echo "$SERVICES_TO_RECREATE" | grep -q "pms-gateway"; then
    echo -e "\${YELLOW}   Restarting Caddy (depends on gateway)...\${NC}"
    docker compose -f $COMPOSE_FILE up -d --force-recreate caddy 2>&1 || true
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
