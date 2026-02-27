#!/bin/bash
# =============================================================================
# Script de deploiement PMS Testnet (Engine + Gateway + Caddy + Simulator)
# =============================================================================
# Usage: ./deploy-testnet.sh <VPS_IP> [USER] [SSH_KEY]
#
# SSH key auth recommended to avoid password prompts during long builds:
#   ssh-keygen -t ed25519 -f ~/.ssh/pms_vps
#   ssh-copy-id -i ~/.ssh/pms_vps pms@<VPS_IP>
#   ./deploy-testnet.sh <VPS_IP> pms ~/.ssh/pms_vps
#
# Architecture:
#   Internet -> Caddy (80/443, Let's Encrypt testnet.pms-network.com)
#                -> Gateway (8443, self-signed TLS interne)
#                    -> Engine (8080, internal only)
#              Prometheus (9091, localhost only)
#              Simulator -> Gateway (97 agents, dashboard :9090)
#
# Les images Docker sont buildees EN LOCAL (cross-compile linux/amd64)
# puis transferees au VPS via scp + docker load.
# Plus aucun build Rust sur le VPS.

set -e

VPS_IP="${1:-}"
VPS_USER="${2:-pms}"
SSH_KEY="${3:-}"

# SSH options: use key if provided, keep connection alive during long builds
SSH_OPTS="-o ServerAliveInterval=30 -o ServerAliveCountMax=5 -o ConnectTimeout=10"
if [ -n "$SSH_KEY" ]; then
    SSH_OPTS="$SSH_OPTS -i $SSH_KEY"
fi

DOMAIN_NAME="testnet.pms-network.com"
COMPOSE_FILE="docker-compose.testnet.yml"
CONFIG_FILE="etc/config/config.testnet.toml"
REMOTE_DIR="/opt/pms"
LOCAL_IMG_DIR="/tmp/pms-images"

# Couleurs
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
echo -e "${CYAN}   PMS Testnet Deployment (Local Build)${NC}"
echo -e "${CYAN}===============================================${NC}"
echo "   Target: $VPS_USER@$VPS_IP"
echo "   Domain: $DOMAIN_NAME"
echo "   Stack:  Engine + Gateway + Caddy + Prometheus + Simulator"
echo -e "   Build:  ${BOLD}Local (cross-compile linux/amd64)${NC}"
echo ""

# -----------------------------------------------------------------------------
# Fonctions Utilitaires
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
# 1. Token Admin
# -----------------------------------------------------------------------------
echo -e "${YELLOW}[1/7] Admin Token${NC}"
read -p "   Enter ADMIN_TOKEN (leave empty to generate random): " ADMIN_TOKEN

if [ -z "$ADMIN_TOKEN" ]; then
    ADMIN_TOKEN=$(openssl rand -hex 32)
    echo -e "   Generated: ${GREEN}$ADMIN_TOKEN${NC}"
fi

# -----------------------------------------------------------------------------
# 2. Collecte des intentions
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}[2/7] Select Actions${NC}"

DO_BUILD=false
if ask_yes_no "   Build & deploy Docker images?" "Y"; then
    DO_BUILD=true
fi

DO_CLEAN_RESET=false
if ask_yes_no "   [DANGER] Clean Reset (delete ALL testnet data/volumes)?" "N"; then
    DO_CLEAN_RESET=true
fi

DO_INIT_COORD=false
if ask_yes_no "   Initialize Coordinator (required on first deploy)?" "N"; then
    DO_INIT_COORD=true
fi

# -----------------------------------------------------------------------------
# 3. Local Build (cross-compile for linux/amd64)
# -----------------------------------------------------------------------------
if [ "$DO_BUILD" = "true" ]; then
    echo ""
    echo -e "${YELLOW}[3/7] Building images locally (linux/amd64)...${NC}"
    echo -e "   ${BOLD}This runs on YOUR machine, not the VPS.${NC}"

    mkdir -p "$LOCAL_IMG_DIR"

    # Setup buildx builder for cross-platform
    if ! docker buildx inspect pms-builder >/dev/null 2>&1; then
        echo -e "   Creating buildx builder..."
        docker buildx create --name pms-builder --use >/dev/null 2>&1
    else
        docker buildx use pms-builder >/dev/null 2>&1
    fi

    # Build Engine (pms-node:testnet)
    echo ""
    echo -e "   ${CYAN}[1/3] Building Engine...${NC}"
    docker buildx build \
        --platform linux/amd64 \
        -t pms-node:testnet \
        --output "type=docker,dest=$LOCAL_IMG_DIR/pms-node.tar" \
        .
    echo -e "   ${GREEN}Engine built.${NC}"

    # Build Gateway (pms-gateway:testnet)
    echo ""
    echo -e "   ${CYAN}[2/3] Building Gateway...${NC}"
    docker buildx build \
        --platform linux/amd64 \
        -t pms-gateway:testnet \
        -f Dockerfile.gateway \
        --output "type=docker,dest=$LOCAL_IMG_DIR/pms-gateway.tar" \
        .
    echo -e "   ${GREEN}Gateway built.${NC}"

    # Build Simulator (pms-simulator:testnet)
    echo ""
    echo -e "   ${CYAN}[3/3] Building Simulator...${NC}"
    docker buildx build \
        --platform linux/amd64 \
        -t pms-simulator:testnet \
        -f Dockerfile.simulator \
        --output "type=docker,dest=$LOCAL_IMG_DIR/pms-simulator.tar" \
        .
    echo -e "   ${GREEN}Simulator built.${NC}"

    # Compress
    echo ""
    echo -e "   Compressing images..."
    for f in "$LOCAL_IMG_DIR"/*.tar; do
        gzip -f "$f"
    done

    TOTAL_SIZE=$(du -sh "$LOCAL_IMG_DIR" | awk '{print $1}')
    echo -e "   ${GREEN}All images built and compressed. Total: $TOTAL_SIZE${NC}"
else
    echo ""
    echo -e "${YELLOW}[3/7] Skipping build.${NC}"
fi

# -----------------------------------------------------------------------------
# 4. Transfer files to VPS
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}[4/7] Transferring files to VPS...${NC}"

# Ensure remote directories exist
ssh -T $SSH_OPTS "$VPS_USER@$VPS_IP" "mkdir -p $REMOTE_DIR/etc/config $REMOTE_DIR/etc/pms $REMOTE_DIR/secrets/tls $REMOTE_DIR/etc/prometheus $REMOTE_DIR/tools/simulator"

# Transfer config files (always — they may have changed locally)
echo -e "   Uploading config files..."
scp -q $SSH_OPTS "$COMPOSE_FILE" "$VPS_USER@$VPS_IP:$REMOTE_DIR/$COMPOSE_FILE"

# If NOT re-initializing coordinator, preserve VPS-specific config values
# (gen-coordinator writes them on init; local copy has them empty)
if [ "$DO_INIT_COORD" = "false" ]; then
    SAVED_COORD_KEY=$(ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
        "grep '^coordinator_public_key' $REMOTE_DIR/$CONFIG_FILE 2>/dev/null" || echo "")
    SAVED_X25519_KEY=$(ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
        "grep '^coordinator_x25519_public_key' $REMOTE_DIR/$CONFIG_FILE 2>/dev/null" || echo "")
    SAVED_WALLET_ADDRS=$(ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
        "grep '^wallet_addresses' $REMOTE_DIR/$CONFIG_FILE 2>/dev/null" || echo "")
    SAVED_TREASURY_ADDRS=$(ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
        "grep '^treasury_addresses' $REMOTE_DIR/$CONFIG_FILE 2>/dev/null" || echo "")
fi

scp -q $SSH_OPTS "$CONFIG_FILE" "$VPS_USER@$VPS_IP:$REMOTE_DIR/$CONFIG_FILE"

# Restore VPS-specific config values if we saved them
if [ "$DO_INIT_COORD" = "false" ] && [ -n "$SAVED_COORD_KEY" ]; then
    echo -e "   Preserving VPS config values..."
    ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" "
        sed -i 's|^coordinator_public_key = .*|${SAVED_COORD_KEY}|' $REMOTE_DIR/$CONFIG_FILE
        sed -i 's|^coordinator_x25519_public_key = .*|${SAVED_X25519_KEY}|' $REMOTE_DIR/$CONFIG_FILE
    "
    # Restore wallet_addresses (fee recipient)
    if [ -n "$SAVED_WALLET_ADDRS" ]; then
        ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" \
            "sed -i 's|^wallet_addresses = .*|${SAVED_WALLET_ADDRS}|' $REMOTE_DIR/$CONFIG_FILE"
    fi
    # Restore treasury_addresses (fee distribution)
    if [ -n "$SAVED_TREASURY_ADDRS" ]; then
        ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" "
            if grep -q '^treasury_addresses' $REMOTE_DIR/$CONFIG_FILE; then
                sed -i 's|^treasury_addresses = .*|${SAVED_TREASURY_ADDRS}|' $REMOTE_DIR/$CONFIG_FILE
            else
                sed -i '/^\[fees\]/a ${SAVED_TREASURY_ADDRS}' $REMOTE_DIR/$CONFIG_FILE
            fi
        "
    fi
    echo -e "   ${GREEN}VPS config values preserved.${NC}"
fi

scp -q $SSH_OPTS tools/simulator/simulator.testnet.toml "$VPS_USER@$VPS_IP:$REMOTE_DIR/tools/simulator/simulator.testnet.toml"
scp -q $SSH_OPTS tools/simulator/agents_testnet.toml "$VPS_USER@$VPS_IP:$REMOTE_DIR/tools/simulator/agents_testnet.toml"
echo -e "   ${GREEN}Config files uploaded.${NC}"

# Transfer Docker images (only if we built them)
if [ "$DO_BUILD" = "true" ]; then
    echo -e "   Uploading Docker images to VPS (this may take a few minutes)..."
    ssh -T $SSH_OPTS "$VPS_USER@$VPS_IP" "mkdir -p /tmp/pms-images"

    for img in pms-node pms-gateway pms-simulator; do
        SIZE=$(du -sh "$LOCAL_IMG_DIR/$img.tar.gz" | awk '{print $1}')
        echo -e "   Uploading $img ($SIZE)..."
        scp $SSH_OPTS "$LOCAL_IMG_DIR/$img.tar.gz" "$VPS_USER@$VPS_IP:/tmp/pms-images/"
    done

    echo -e "   ${GREEN}All images uploaded.${NC}"

    # Cleanup local tarballs
    rm -rf "$LOCAL_IMG_DIR"
fi

# -----------------------------------------------------------------------------
# 5. Remote: Load images + Setup + Deploy
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}[5/7] Deploying on VPS...${NC}"

ssh -T $SSH_OPTS "$VPS_USER@$VPS_IP" << EOFREMOTE
set -e

ADMIN_TOKEN="$ADMIN_TOKEN"
DOMAIN_NAME="$DOMAIN_NAME"
COMPOSE_FILE="$COMPOSE_FILE"
CONFIG_FILE="$CONFIG_FILE"
DO_BUILD="$DO_BUILD"
DO_CLEAN_RESET="$DO_CLEAN_RESET"
DO_INIT_COORD="$DO_INIT_COORD"

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

cd $REMOTE_DIR

# --- Load Docker images ---
if [ "\$DO_BUILD" = "true" ]; then
    echo -e "\${YELLOW}   Loading Docker images...\${NC}"
    for img in /tmp/pms-images/*.tar.gz; do
        NAME=\$(basename "\$img" .tar.gz)
        echo -n "   Loading \$NAME... "
        gunzip -c "\$img" | docker load 2>&1 | tail -1
    done
    rm -rf /tmp/pms-images
    echo -e "\${GREEN}   All images loaded.\${NC}"

    # Prune dangling images to free disk space on VPS
    echo -e "\${YELLOW}   Pruning old Docker images...\${NC}"
    docker image prune -f 2>/dev/null || true
    echo -e "\${GREEN}   Docker prune done.\${NC}"
fi

# --- Setup secrets (only if missing) ---
echo -e "\${YELLOW}   Checking secrets...\${NC}"

if [ ! -f etc/pms/node.key ]; then
    openssl rand -hex 32 > etc/pms/node.key
    chmod 600 etc/pms/node.key
    echo -e "   \${GREEN}Generated node.key\${NC}"
fi

if [ ! -f etc/pms/api-keys.json ]; then
    echo '{"keys":[]}' > etc/pms/api-keys.json
    chmod 600 etc/pms/api-keys.json
    echo -e "   \${GREEN}Created empty api-keys.json\${NC}"
fi

if [ ! -f secrets/tls/cert.pem ]; then
    echo -e "   \${YELLOW}Generating TLS certificates...\${NC}"
    openssl req -x509 -newkey rsa:4096 -keyout secrets/tls/key.pem \
        -out secrets/tls/cert.pem -days 365 -nodes \
        -subj "/CN=pms-testnet" \
        -addext "subjectAltName=IP:127.0.0.1,DNS:localhost,DNS:pms-engine,DNS:pms-gateway" \
        2>/dev/null
    chmod 644 secrets/tls/*.pem
    echo -e "   \${GREEN}TLS certificates generated\${NC}"
fi

if [ ! -f etc/prometheus/prometheus.yml ]; then
    cat > etc/prometheus/prometheus.yml << 'PROM_EOF'
global:
  scrape_interval: 15s
  evaluation_interval: 15s

scrape_configs:
  - job_name: 'pms-engine'
    scheme: https
    tls_config:
      insecure_skip_verify: true
    static_configs:
      - targets: ['pms-engine:8080']
    metrics_path: /metrics

  - job_name: 'pms-gateway'
    scheme: https
    tls_config:
      insecure_skip_verify: true
    static_configs:
      - targets: ['pms-gateway:8443']
    metrics_path: /metrics
PROM_EOF
    echo -e "   \${GREEN}Prometheus config created\${NC}"
fi

# --- Caddyfile ---
cat > Caddyfile.testnet << CADDY_EOF
{
    email admin@pms-network.com
}

\$DOMAIN_NAME {
    tls admin@pms-network.com
    reverse_proxy https://pms-gateway:8443 {
        transport http {
            tls
            tls_insecure_skip_verify
        }
    }
}
CADDY_EOF
echo -e "   \${GREEN}Caddyfile.testnet generated\${NC}"

# --- Pre-flight check ---
echo ""
echo -e "\${YELLOW}   Pre-flight check...\${NC}"
echo "   ==================================================="

MISSING_CRITICAL=false

check_file() {
    local file=\$1
    local is_critical=\$2
    if [ -f "\$file" ]; then
        printf "   %-45s \${GREEN}[OK]\${NC}\n" "\$file"
    else
        printf "   %-45s \${RED}[MISSING]\${NC}\n" "\$file"
        if [ "\$is_critical" = "true" ]; then
            MISSING_CRITICAL=true
        fi
    fi
}

check_file "\$COMPOSE_FILE" "true"
check_file "\$CONFIG_FILE" "true"
check_file "etc/pms/node.key" "true"
check_file "etc/pms/api-keys.json" "true"
check_file "secrets/tls/cert.pem" "true"
check_file "Caddyfile.testnet" "true"
check_file "etc/prometheus/prometheus.yml" "true"
check_file "tools/simulator/simulator.testnet.toml" "true"
check_file "tools/simulator/agents_testnet.toml" "true"

# Treasury only critical if NOT initializing coordinator
if [ "\$DO_INIT_COORD" = "true" ]; then
    check_file "etc/pms/coordinator.json" "false"
    check_file "etc/pms/treasury-wallets.json" "false"
else
    check_file "etc/pms/coordinator.json" "true"
    check_file "etc/pms/treasury-wallets.json" "true"
fi

echo "   ==================================================="

if [ "\$MISSING_CRITICAL" = "true" ]; then
    echo -e "   \${RED}CRITICAL FILES MISSING! Stopping.\${NC}"
    exit 1
else
    echo -e "   \${GREEN}All critical files present.\${NC}"
fi

echo ""

# --- Deploy ---
if [ "\$DO_BUILD" = "true" ]; then

    # Clean reset if requested
    if [ "\$DO_CLEAN_RESET" = "true" ]; then
        echo -e "\${RED}   Cleaning ALL testnet data...\${NC}"
        PMS_ADMIN_TOKEN="\$ADMIN_TOKEN" docker compose -f \$COMPOSE_FILE down -v --remove-orphans 2>/dev/null || true
    else
        PMS_ADMIN_TOKEN="\$ADMIN_TOKEN" docker compose -f \$COMPOSE_FILE down --remove-orphans 2>/dev/null || true
    fi

    # Remove old containers
    docker rm -f pms-engine-testnet pms-gateway-testnet pms-caddy-testnet pms-prometheus-testnet pms-simulator-testnet 2>/dev/null || true

    # --- Coordinator Init ---
    if [ "\$DO_INIT_COORD" = "true" ]; then
        echo ""
        echo -e "\${YELLOW}   Initializing Coordinator...\${NC}"

        chmod 755 etc/pms
        chmod 755 etc/config

        # 1. Gen Coordinator
        PMS_ADMIN_TOKEN="\$ADMIN_TOKEN" docker compose -f \$COMPOSE_FILE run --rm --entrypoint /bin/bash pms-engine -c \
            "/usr/local/bin/tools-cli gen-coordinator \
            /home/pms/config/pms/coordinator.key \
            /home/pms/config/pms/coordinator.json \
            /home/pms/config/config.testnet.toml --force"

        # 2. Gen Treasury
        echo -e "\${YELLOW}   Generating Treasury...\${NC}"
        PMS_ADMIN_TOKEN="\$ADMIN_TOKEN" docker compose -f \$COMPOSE_FILE run --rm --entrypoint /bin/bash pms-engine -c \
            "/usr/local/bin/tools-cli treasury-generate 1 \
            /home/pms/config/pms/treasury-keys \
            /home/pms/config/pms/treasury-wallets.json"

        # 3. Sign Treasury
        PMS_ADMIN_TOKEN="\$ADMIN_TOKEN" docker compose -f \$COMPOSE_FILE run --rm --entrypoint /bin/bash pms-engine -c \
            "/usr/local/bin/tools-cli treasury-sign \
            /home/pms/config/pms/coordinator.key \
            /home/pms/config/pms/treasury-wallets.json"

        # 4. Copy coordinator.key as node.key
        echo -e "\${YELLOW}   Setting node identity = coordinator key...\${NC}"
        cp etc/pms/coordinator.key etc/pms/node.key
        chmod 600 etc/pms/node.key
        echo -e "\${GREEN}   node.key = coordinator.key\${NC}"

        # 5. Inject fee recipient addresses into config
        echo -e "\${YELLOW}   Injecting fee recipient addresses into config...\${NC}"
        COORD_ADDR=\$(python3 -c "import json; print(json.load(open('etc/pms/coordinator.json'))['address'])" 2>/dev/null || echo "")
        TREASURY_ADDR=\$(python3 -c "import json; d=json.load(open('etc/pms/treasury-wallets.json')); print(d[0]['address'] if isinstance(d,list) and d else d.get('wallets',[])[0]['address'] if 'wallets' in d else '')" 2>/dev/null || echo "")

        if [ -n "\$COORD_ADDR" ]; then
            # Set admin wallet_addresses = [coordinator address]
            sed -i "s|^wallet_addresses = \\[\\]|wallet_addresses = [\"\$COORD_ADDR\"]|" \$CONFIG_FILE
            echo -e "   \${GREEN}admin.wallet_addresses = [\$COORD_ADDR]\${NC}"
        fi

        if [ -n "\$TREASURY_ADDR" ]; then
            # Add treasury_addresses under [fees] section
            if ! grep -q 'treasury_addresses' \$CONFIG_FILE; then
                sed -i "/^\\[fees\\]/a treasury_addresses = [\"\$TREASURY_ADDR\"]" \$CONFIG_FILE
                echo -e "   \${GREEN}fees.treasury_addresses = [\$TREASURY_ADDR]\${NC}"
            fi
        fi

        # Verify coordinator key
        echo -e "\${YELLOW}   Verifying coordinator key...\${NC}"
        if grep -q 'coordinator_public_key = ""' \$CONFIG_FILE; then
            echo -e "\${RED}   ERROR: Config NOT updated with coordinator key!\${NC}"
            exit 1
        else
            echo -e "\${GREEN}   Coordinator initialized successfully.\${NC}"
        fi
    fi

    # Start all services
    echo ""
    echo -e "\${YELLOW}   Starting all services...\${NC}"
    export PMS_ADMIN_TOKEN="\$ADMIN_TOKEN"
    docker compose -f \$COMPOSE_FILE up -d --force-recreate

    echo -e "\${GREEN}   Containers started.\${NC}"

    # Wait for Engine
    echo "   Waiting for Engine..."
    for i in \$(seq 1 30); do
        if docker exec pms-engine-testnet bash -c 'timeout 2 bash -c "</dev/tcp/127.0.0.1/8080"' 2>/dev/null; then
            echo -e "   \${GREEN}Engine is UP\${NC}"
            break
        fi
        if [ \$i -eq 30 ]; then
            echo -e "   \${RED}Engine failed to start!\${NC}"
            docker compose -f \$COMPOSE_FILE logs --tail 50 pms-engine
            exit 1
        fi
        echo -n "."
        sleep 1
    done

    # Wait for Gateway
    echo "   Waiting for Gateway..."
    for i in \$(seq 1 30); do
        if docker exec pms-gateway-testnet bash -c 'timeout 2 bash -c "</dev/tcp/127.0.0.1/8443"' 2>/dev/null; then
            echo -e "   \${GREEN}Gateway is UP\${NC}"
            break
        fi
        if [ \$i -eq 30 ]; then
            echo -e "   \${RED}Gateway failed to start!\${NC}"
            docker compose -f \$COMPOSE_FILE logs --tail 50 pms-gateway
            exit 1
        fi
        echo -n "."
        sleep 1
    done

    # --- Create SDK API Key (before simulator can work) ---
    # Engine port 8080 is internal-only (Docker expose, not ports).
    # Use docker exec in the gateway container (has curl + is on internal network).
    echo ""
    echo -e "\${YELLOW}   Creating SDK API Key...\${NC}"

    API_KEY_RESPONSE=\$(docker compose -f \$COMPOSE_FILE exec -T pms-gateway \
        curl -sk -X POST \
        -H "Authorization: Bearer \$ADMIN_TOKEN" \
        -H "Content-Type: application/json" \
        -d '{"label": "SDK Default", "scopes": ["*"]}' \
        https://pms-engine:8080/admin/api-keys 2>/dev/null || echo "")

    if echo "\$API_KEY_RESPONSE" | grep -q '"key"'; then
        SDK_API_KEY=\$(echo "\$API_KEY_RESPONSE" | grep -o '"key"\s*:\s*"[^"]*"' | awk -F'"' '{print \$4}' 2>/dev/null || echo "")
        echo "\$API_KEY_RESPONSE" > etc/pms/sdk-api-key.json
        chmod 600 etc/pms/sdk-api-key.json
        echo -e "   \${GREEN}SDK API Key created: \${SDK_API_KEY:0:20}...\${NC}"
        echo -e "   \${YELLOW}Save this key! It is shown only ONCE.\${NC}"

        # Restart simulator with the API key so it can authenticate
        echo -e "   \${YELLOW}Restarting simulator with API key...\${NC}"
        export PMS_API_KEY="\$SDK_API_KEY"
        docker compose -f \$COMPOSE_FILE up -d --force-recreate pms-simulator
    else
        echo -e "   \${RED}Could not create API key. Response: \${API_KEY_RESPONSE}\${NC}"
        echo -e "   \${YELLOW}Simulator will not work without an API key.\${NC}"
    fi

    # Wait for Simulator (now has API key)
    echo "   Waiting for Simulator..."
    for i in \$(seq 1 30); do
        if curl -sf http://127.0.0.1:9090/ > /dev/null 2>&1; then
            echo -e "   \${GREEN}Simulator is UP\${NC}"
            break
        fi
        if [ \$i -eq 30 ]; then
            echo -e "   \${YELLOW}Simulator not responding yet.\${NC}"
            echo "   Check logs: docker logs -f pms-simulator-testnet"
        fi
        echo -n "."
        sleep 1
    done
    echo ""

    # Final status
    echo -e "\${YELLOW}   Service Status:\${NC}"
    docker compose -f \$COMPOSE_FILE ps

else
    echo "   Skipping deploy (no build requested)."
fi

EOFREMOTE

# -----------------------------------------------------------------------------
# 6. Secure Backup
# -----------------------------------------------------------------------------
echo ""
if ask_yes_no "   Download Secure Backup (coordinator keys) locally?" "Y"; then
    echo -e "${YELLOW}[6/7] Downloading backup...${NC}"

    BACKUP_DIR="backups"
    DEFAULT_FILENAME="pms-testnet-$(date +%Y%m%d-%H%M%S).json"
    BACKUP_FILE=""

    if command -v osascript &>/dev/null; then
        BACKUP_FILE=$(osascript -e "set fileName to choose file name with prompt \"Save PMS Testnet Backup:\" default name \"$DEFAULT_FILENAME\" default location (path to desktop folder)" -e "POSIX path of fileName" 2>/dev/null)
    fi

    if [ -z "$BACKUP_FILE" ]; then
        mkdir -p "$BACKUP_DIR"
        BACKUP_FILE="$BACKUP_DIR/$DEFAULT_FILENAME"
    fi

    TMP_DIR=$(mktemp -d)

    scp -q $SSH_OPTS "$VPS_USER@$VPS_IP:$REMOTE_DIR/etc/pms/coordinator.json" "$TMP_DIR/coordinator.json" 2>/dev/null || echo "{}" > "$TMP_DIR/coordinator.json"
    scp -q $SSH_OPTS "$VPS_USER@$VPS_IP:$REMOTE_DIR/etc/pms/coordinator.key" "$TMP_DIR/coordinator.key" 2>/dev/null || touch "$TMP_DIR/coordinator.key"
    scp -q $SSH_OPTS "$VPS_USER@$VPS_IP:$REMOTE_DIR/etc/pms/treasury-wallets.json" "$TMP_DIR/treasury-wallets.json" 2>/dev/null || echo "[]" > "$TMP_DIR/treasury-wallets.json"

    mkdir -p "$TMP_DIR/treasury-keys"
    scp -q $SSH_OPTS -r "$VPS_USER@$VPS_IP:$REMOTE_DIR/etc/pms/treasury-keys/*" "$TMP_DIR/treasury-keys/" 2>/dev/null || true
    scp -q $SSH_OPTS "$VPS_USER@$VPS_IP:$REMOTE_DIR/etc/pms/sdk-api-key.json" "$TMP_DIR/sdk-api-key.json" 2>/dev/null || echo "{}" > "$TMP_DIR/sdk-api-key.json"

    # Clean up the temporary API key file on the VPS for security
    ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" "rm -f $REMOTE_DIR/etc/pms/sdk-api-key.json" 2>/dev/null

    python3 -c "
import json, os, glob

def read_file(path, default):
    if not os.path.exists(path): return default
    with open(path, 'r') as f:
        content = f.read().strip()
        return content if content else default

coord_json = json.loads(read_file('$TMP_DIR/coordinator.json', '{}'))
coord_key = read_file('$TMP_DIR/coordinator.key', '')
treasury = json.loads(read_file('$TMP_DIR/treasury-wallets.json', '[]'))

treasury_keys = []
for kf in glob.glob('$TMP_DIR/treasury-keys/*.json'):
    try:
        with open(kf, 'r') as f:
            treasury_keys.append(json.load(f))
    except: pass

sdk_api_key = json.loads(read_file('$TMP_DIR/sdk-api-key.json', '{}'))

data = {
    'deployment': {
        'type': 'testnet',
        'domain': '$DOMAIN_NAME',
        'admin_token': '$ADMIN_TOKEN',
        'vps_ip': '$VPS_IP',
        'user': '$VPS_USER'
    },
    'coordinator': {
        'wallet': coord_json,
        'private_key_hex': coord_key
    },
    'treasury_public': treasury,
    'treasury_keys': treasury_keys,
    'sdk_api_key': sdk_api_key
}
print(json.dumps(data, indent=2))
" > "$BACKUP_FILE"

    rm -rf "$TMP_DIR"

    echo -e "   ${GREEN}Backup saved: $BACKUP_FILE${NC}"
    echo -e "   ${RED}KEEP THIS FILE SECRET — contains private keys!${NC}"
else
    echo -e "${YELLOW}[6/7] Skipping backup.${NC}"
fi

# -----------------------------------------------------------------------------
# 7. Connectivity Check
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}[7/7] Connectivity check...${NC}"

echo "   Testing: https://$DOMAIN_NAME/livez"
if HTTP_STATUS=$(curl -sk -o /dev/null -w "%{http_code}" "https://$DOMAIN_NAME/livez" 2>/dev/null); then
    if [ "$HTTP_STATUS" == "200" ]; then
        echo -e "   ${GREEN}$HTTP_STATUS OK — Testnet is live!${NC}"
    else
        echo -e "   ${YELLOW}$HTTP_STATUS — Server reachable but returned error.${NC}"
    fi
else
    echo -e "   ${RED}Connection failed (DNS propagating or firewall).${NC}"
fi

# -----------------------------------------------------------------------------
# Final Recap: fetch coordinator wallet info from VPS
# -----------------------------------------------------------------------------
COORD_JSON=$(ssh -q $SSH_OPTS "$VPS_USER@$VPS_IP" "cat $REMOTE_DIR/etc/pms/coordinator.json 2>/dev/null" || echo "{}")

COORD_ADDRESS=""
COORD_PRIVKEY=""
COORD_PUBKEY=""
COORD_MNEMONIC=""

if [ -n "$COORD_JSON" ] && [ "$COORD_JSON" != "{}" ]; then
    COORD_ADDRESS=$(echo "$COORD_JSON" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('address',''))" 2>/dev/null || echo "")
    COORD_PRIVKEY=$(echo "$COORD_JSON" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('private_key',''))" 2>/dev/null || echo "")
    COORD_PUBKEY=$(echo "$COORD_JSON" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('public_key',''))" 2>/dev/null || echo "")
    COORD_MNEMONIC=$(echo "$COORD_JSON" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('mnemonic','') or '')" 2>/dev/null || echo "")
fi

echo ""
echo -e "${CYAN}===============================================${NC}"
echo -e "${CYAN}   PMS Testnet Deployment Complete${NC}"
echo -e "${CYAN}===============================================${NC}"
echo ""
echo "   Services:"
echo "   - Testnet API:     https://$DOMAIN_NAME"
echo "   - Dashboard:       https://$DOMAIN_NAME/dashboard/"
echo "   - Simulator:       http://$VPS_IP:9090"
echo "   - Prometheus:      http://localhost:9091 (VPS only)"
echo ""
if [ -n "$COORD_ADDRESS" ]; then
    echo -e "   ${BOLD}Coordinator Wallet:${NC}"
    echo -e "   - Address:     ${GREEN}$COORD_ADDRESS${NC}"
    echo -e "   - Private Key: ${RED}$COORD_PRIVKEY${NC}"
    echo -e "   - Public Key:  $COORD_PUBKEY"
    if [ -n "$COORD_MNEMONIC" ]; then
        echo -e "   - Mnemonic:    ${RED}$COORD_MNEMONIC${NC}"
    else
        echo -e "   - Mnemonic:    ${YELLOW}(not available — generated from raw key)${NC}"
    fi
    echo ""
    echo -e "   ${RED}KEEP PRIVATE KEY & MNEMONIC SECRET!${NC}"
    echo ""
fi
echo "   Useful commands (on VPS):"
echo "   - Logs engine:     docker logs -f pms-engine-testnet"
echo "   - Logs simulator:  docker logs -f pms-simulator-testnet"
echo "   - Status:          docker compose -f $COMPOSE_FILE ps"
echo "   - Stop:            PMS_ADMIN_TOKEN=xxx docker compose -f $COMPOSE_FILE down"
echo ""
