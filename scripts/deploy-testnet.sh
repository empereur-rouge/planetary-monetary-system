#!/bin/bash
# =============================================================================
# Script de deploiement PMS Testnet (Engine + Gateway + Caddy + Simulator)
# =============================================================================
# Usage: ./deploy-testnet.sh <VPS_IP> [USER]
#
# Architecture:
#   Internet -> Caddy (80/443, Let's Encrypt testnet.pms-network.com)
#                -> Gateway (8443, self-signed TLS interne)
#                    -> Engine (8080, internal only)
#              Prometheus (9091, localhost only)
#              Simulator -> Gateway (97 agents, dashboard :9090)
#
# Ce script guide l'utilisateur a travers les etapes de deploiement testnet:
# 1. Configuration (Admin Token)
# 2. Mise a jour (Git Pull)
# 3. Build & Restart (Docker)
# 4. Initialisation Coordinateur (Premier deploiement)

set -e

VPS_IP="${1:-}"
VPS_USER="${2:-pms}"

DOMAIN_NAME="testnet.pms-network.com"
COMPOSE_FILE="docker-compose.testnet.yml"
CONFIG_FILE="etc/config/config.testnet.toml"

# Couleurs
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
CYAN='\033[0;36m'
NC='\033[0m'

if [ -z "$VPS_IP" ]; then
    echo "Usage: $0 <VPS_IP> [USER]"
    echo "Example: $0 87.106.50.82 pms"
    exit 1
fi

echo -e "${CYAN}===============================================${NC}"
echo -e "${CYAN}   PMS Testnet Deployment${NC}"
echo -e "${CYAN}===============================================${NC}"
echo "   Target: $VPS_USER@$VPS_IP"
echo "   Domain: $DOMAIN_NAME"
echo "   Stack:  Engine + Gateway + Caddy + Prometheus + Simulator"
echo ""
echo -e "${RED}   Make sure you have committed and pushed your changes.${NC}"
echo -e "${RED}   DNS: A record for $DOMAIN_NAME must point to $VPS_IP${NC}"
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
echo -e "${YELLOW}[1/6] Admin Token${NC}"
read -p "   Enter ADMIN_TOKEN (leave empty to generate random): " ADMIN_TOKEN

if [ -z "$ADMIN_TOKEN" ]; then
    ADMIN_TOKEN=$(openssl rand -hex 32)
    echo -e "   Generated: ${GREEN}$ADMIN_TOKEN${NC}"
fi

# -----------------------------------------------------------------------------
# 2. Collecte des intentions
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}[2/6] Select Actions${NC}"

DO_GIT_PULL=false
if ask_yes_no "   Git Pull (update code from GitHub)?" "Y"; then
    DO_GIT_PULL=true
fi

DO_BUILD=false
if ask_yes_no "   Rebuild & Restart Docker containers?" "Y"; then
    DO_BUILD=true
fi

DO_CLEAN_RESET=false
if ask_yes_no "   [DANGER] Clean Reset (delete ALL testnet data/volumes)?" "N"; then
    DO_CLEAN_RESET=true
fi

DO_INIT_COORD=false
if ask_yes_no "   Initialize Coordinator (required on first deploy)?" "Y"; then
    DO_INIT_COORD=true
fi

# -----------------------------------------------------------------------------
# 3. SSH Phase 1: Git Update
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}[3/6] Git Update...${NC}"

ssh -T $VPS_USER@$VPS_IP << EOFREMOTE_GIT
set -e
DO_GIT_PULL="$DO_GIT_PULL"

YELLOW='\033[1;33m'
GREEN='\033[0;32m'
NC='\033[0m'

mkdir -p /opt/pms
cd /opt/pms

if [ "\$DO_GIT_PULL" = "true" ]; then
    echo -e "\${YELLOW}   Updating repository...\${NC}"
    if [ -d ".git" ]; then
        git reset --hard HEAD
        git pull
    else
        git clone https://github.com/empereur-rouge/planetary-monetary-system.git .
    fi
    echo -e "\${GREEN}   Done.\${NC}"
else
    echo "   Skipping Git Pull."
fi
EOFREMOTE_GIT

# -----------------------------------------------------------------------------
# 4. SSH Phase 2: Config & Deploy
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}[4/6] Configure & Deploy...${NC}"

ssh -T $VPS_USER@$VPS_IP << EOFREMOTE_MAIN
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

cd /opt/pms

# --- Setup Directories ---
echo -e "\${YELLOW}   Setting up directories...\${NC}"
mkdir -p etc/pms secrets/tls etc/prometheus etc/config

# --- node.key ---
if [ ! -f etc/pms/node.key ]; then
    openssl rand -hex 32 > etc/pms/node.key
    chmod 600 etc/pms/node.key
    echo -e "   \${GREEN}Generated node.key\${NC}"
fi

# api-keys.json (SDK API key store)
if [ ! -f etc/pms/api-keys.json ]; then
    echo '{"keys":[]}' > etc/pms/api-keys.json
    chmod 600 etc/pms/api-keys.json
    echo -e "   \${GREEN}Created empty api-keys.json\${NC}"
fi

# --- TLS certificates ---
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

# --- Prometheus config ---
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

# --- Caddyfile.testnet ---
echo -e "   \${YELLOW}Generating Caddyfile.testnet...\${NC}"
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
echo -e "   \${GREEN}Caddyfile.testnet generated -> \$DOMAIN_NAME\${NC}"

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
check_file "Dockerfile" "true"
check_file "Dockerfile.gateway" "true"
check_file "Dockerfile.simulator" "true"
check_file "\$CONFIG_FILE" "true"
check_file "etc/pms/node.key" "true"
check_file "secrets/tls/cert.pem" "true"
check_file "Caddyfile.testnet" "true"
check_file "etc/prometheus/prometheus.yml" "true"
check_file "pms-dashboard/package.json" "true"
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

# --- Build & Deploy ---
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

    # Build images
    echo -e "\${YELLOW}   Building Engine...\${NC}"
    PMS_ADMIN_TOKEN="\$ADMIN_TOKEN" docker compose -f \$COMPOSE_FILE build pms-engine

    echo -e "\${YELLOW}   Building Gateway...\${NC}"
    PMS_ADMIN_TOKEN="\$ADMIN_TOKEN" docker compose -f \$COMPOSE_FILE build pms-gateway

    echo -e "\${YELLOW}   Building Simulator...\${NC}"
    PMS_ADMIN_TOKEN="\$ADMIN_TOKEN" docker compose -f \$COMPOSE_FILE build pms-simulator

    # --- Coordinator Init ---
    if [ "\$DO_INIT_COORD" = "true" ]; then
        echo ""
        echo -e "\${YELLOW}   Initializing Coordinator...\${NC}"

        # Temp writable permissions
        chmod 777 etc/pms
        chmod 777 etc/config

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

        # 4. Copy coordinator.key as node.key (node must sign with coordinator key)
        echo -e "\${YELLOW}   Setting node identity = coordinator key...\${NC}"
        cp etc/pms/coordinator.key etc/pms/node.key
        chmod 600 etc/pms/node.key
        echo -e "\${GREEN}   node.key = coordinator.key\${NC}"

        # Restore permissions
        chmod 755 etc/pms
        chmod 755 etc/config

        # Verify
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
    for i in {1..30}; do
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
    for i in {1..30}; do
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

    # Wait for Simulator
    echo "   Waiting for Simulator..."
    for i in {1..60}; do
        if curl -sf http://127.0.0.1:9090/ > /dev/null 2>&1; then
            echo -e "   \${GREEN}Simulator is UP\${NC}"
            break
        fi
        if [ \$i -eq 60 ]; then
            echo -e "   \${YELLOW}Simulator not responding yet (may still be bootstrapping agents).\${NC}"
            echo "   Check logs: docker logs -f pms-simulator-testnet"
        fi
        echo -n "."
        sleep 1
    done
    echo ""

    # Final status
    echo -e "\${YELLOW}   Service Status:\${NC}"
    docker compose -f \$COMPOSE_FILE ps

    # --- Create default SDK API Key ---
    echo ""
    echo -e "\${YELLOW}   🔑 Creating SDK API Key...\${NC}"
    API_KEY_RESPONSE=\$(curl -s -X POST \
        -H "Authorization: Bearer \$ADMIN_TOKEN" \
        -H "Content-Type: application/json" \
        -d '{"label": "SDK Default", "scopes": ["*"]}' \
        http://127.0.0.1:8080/admin/api-keys 2>/dev/null || echo "")

    if echo "\$API_KEY_RESPONSE" | grep -q '"key"'; then
        SDK_API_KEY=\$(echo "\$API_KEY_RESPONSE" | python3 -c "import sys,json; print(json.load(sys.stdin)['key'])" 2>/dev/null || echo "")
        echo "\$API_KEY_RESPONSE" > etc/pms/sdk-api-key.json
        chmod 600 etc/pms/sdk-api-key.json
        echo -e "   \${GREEN}✅ SDK API Key created: \${SDK_API_KEY:0:20}...\${NC}"
        echo -e "   \${YELLOW}⚠️  Save this key! It is shown only ONCE.\${NC}"
    else
        echo -e "   \${YELLOW}⚠️  Could not create API key (server may not support it yet).\${NC}"
        echo "   Response: \$API_KEY_RESPONSE"
    fi

else
    echo "   Skipping Build & Restart."
fi

EOFREMOTE_MAIN

# -----------------------------------------------------------------------------
# 5. Secure Backup
# -----------------------------------------------------------------------------
echo ""
if ask_yes_no "   Download Secure Backup (coordinator keys) locally?" "Y"; then
    echo -e "${YELLOW}[5/6] Downloading backup...${NC}"

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

    scp -q $VPS_USER@$VPS_IP:/opt/pms/etc/pms/coordinator.json "$TMP_DIR/coordinator.json" 2>/dev/null || echo "{}" > "$TMP_DIR/coordinator.json"
    scp -q $VPS_USER@$VPS_IP:/opt/pms/etc/pms/coordinator.key "$TMP_DIR/coordinator.key" 2>/dev/null || touch "$TMP_DIR/coordinator.key"
    scp -q $VPS_USER@$VPS_IP:/opt/pms/etc/pms/treasury-wallets.json "$TMP_DIR/treasury-wallets.json" 2>/dev/null || echo "[]" > "$TMP_DIR/treasury-wallets.json"

    mkdir -p "$TMP_DIR/treasury-keys"
    scp -q -r $VPS_USER@$VPS_IP:/opt/pms/etc/pms/treasury-keys/* "$TMP_DIR/treasury-keys/" 2>/dev/null || true
    scp -q $VPS_USER@$VPS_IP:/opt/pms/etc/pms/sdk-api-key.json "$TMP_DIR/sdk-api-key.json" 2>/dev/null || echo "{}" > "$TMP_DIR/sdk-api-key.json"

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
    'treasury_keys': treasury_keys
}
print(json.dumps(data, indent=2))
" > "$BACKUP_FILE"

    rm -rf "$TMP_DIR"

    echo -e "   ${GREEN}Backup saved: $BACKUP_FILE${NC}"
    echo -e "   ${RED}KEEP THIS FILE SECRET — contains private keys!${NC}"
else
    echo -e "${YELLOW}[5/6] Skipping backup.${NC}"
fi

# -----------------------------------------------------------------------------
# 6. Connectivity Check
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}[6/6] Connectivity check...${NC}"

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
echo "   Useful commands (on VPS):"
echo "   - Logs engine:     docker logs -f pms-engine-testnet"
echo "   - Logs simulator:  docker logs -f pms-simulator-testnet"
echo "   - Status:          docker compose -f $COMPOSE_FILE ps"
echo "   - Stop:            PMS_ADMIN_TOKEN=xxx docker compose -f $COMPOSE_FILE down"
echo ""
