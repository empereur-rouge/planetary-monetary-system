#!/bin/bash
# =============================================================================
# Script de déploiement PMS Interactif
# =============================================================================
# Usage: ./deploy.sh <VPS_IP> [USER]
#
# Architecture: Engine + Gateway + Caddy + Prometheus
#
#   Internet → Caddy (80/443, Let's Encrypt)
#                → Gateway (8443, self-signed TLS interne)
#                    → Engine (8080, self-signed TLS interne)
#              Prometheus (9091, localhost only)
#
# Ce script guide l'utilisateur à travers les étapes de déploiement:
# 1. Configuration (Admin Token & Domain)
# 2. Mise à jour (Git Pull)
# 3. Build & Restart (Docker)
# 4. Initialisation Coordinateur (Optionnel)

set -e

VPS_IP="${1:-}"
VPS_USER="${2:-pms}"

# Domaine hardcodé
DOMAIN_NAME="pms-network.com"

# Couleurs
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

if [ -z "$VPS_IP" ]; then
    echo "Usage: $0 <VPS_IP> [USER]"
    echo "Example: $0 87.106.50.82 pms"
    exit 1
fi

echo -e "${YELLOW}🚀 PMS Interactive Deployment Tool${NC}"
echo "   Target: $VPS_USER@$VPS_IP"
echo "   Domain: $DOMAIN_NAME"
echo "   Architecture: Engine + Gateway + Caddy + Prometheus"
echo ""
echo -e "${RED}⚠️  IMPORTANT: Automatic local file sync is DISABLED.${NC}"
echo -e "${RED}    Make sure you have run 'git add . && git commit -m \"...\" && git push'${NC}"
echo -e "${RED}    before continuing, or your local changes will NOT be deployed.${NC}"

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
# 1. Token Admin & Domain
# -----------------------------------------------------------------------------
echo -e "${YELLOW}🔑 Admin Token Configuration${NC}"
read -p "   Enter ADMIN_TOKEN (leave empty to generate random): " ADMIN_TOKEN

if [ -z "$ADMIN_TOKEN" ]; then
    ADMIN_TOKEN=$(openssl rand -hex 32)
    echo -e "   🎲 Generated Token: ${GREEN}$ADMIN_TOKEN${NC}"
fi

# -----------------------------------------------------------------------------
# 2. Collecte des intentions
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}📋 Select Actions${NC}"

DO_GIT_PULL=false
if ask_yes_no "   ❓ Git Pull (Update code from GitHub)?" "Y"; then
    DO_GIT_PULL=true
fi

DO_BUILD=false
if ask_yes_no "   ❓ Rebuild & Restart Docker containers?" "Y"; then
    DO_BUILD=true
fi

DO_CLEAN_RESET=false
if ask_yes_no "   ❓ [DANGER] Clean Reset (Delete ALL data/volumes)?" "N"; then
    DO_CLEAN_RESET=true
fi

DO_INIT_COORD=false
if ask_yes_no "   ❓ Initialize/Reset Coordinator Wallet (DANGEROUS if already set)?" "N"; then
    DO_INIT_COORD=true
fi

# -----------------------------------------------------------------------------
# 3. Execution: Phase 1 (Git Update)
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}🔌 Connecting to VPS (Phase 1: Git Update)...${NC}"

ssh -T $VPS_USER@$VPS_IP << EOFREMOTE_GIT
set -e
DO_GIT_PULL="$DO_GIT_PULL"

# Esthétique distante
YELLOW='\033[1;33m'
NC='\033[0m'

echo "📁 Preparation..."
mkdir -p /opt/pms
cd /opt/pms

# --- Action 1: Git Pull ---
if [ "\$DO_GIT_PULL" = "true" ]; then
    echo -e "\${YELLOW}📥 Updating repository...\${NC}"
    if [ -d ".git" ]; then
        git reset --hard HEAD
        git pull
    else
        git clone https://github.com/empereur-rouge/planetary-monetary-system.git .
    fi
else
    echo "   Skipping Git Pull."
fi
EOFREMOTE_GIT

# -----------------------------------------------------------------------------
# 4. Execution: Phase 2 (Config & Deploy)
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}🔌 Connecting to VPS (Phase 2: Deploy)...${NC}"

ssh -T $VPS_USER@$VPS_IP << EOFREMOTE_MAIN
set -e

# Définition des variables sur le distant
ADMIN_TOKEN="$ADMIN_TOKEN"
DOMAIN_NAME="$DOMAIN_NAME"
DO_BUILD="$DO_BUILD"
DO_CLEAN_RESET="$DO_CLEAN_RESET"
DO_INIT_COORD="$DO_INIT_COORD"

# Esthétique distante
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

cd /opt/pms

# --- Setup Config & Secrets ---
echo -e "\${YELLOW}⚙️  Configuring environment...\${NC}"
mkdir -p etc/pms secrets/tls etc/prometheus

# node.key
if [ ! -f etc/pms/node.key ]; then
    openssl rand -hex 32 > etc/pms/node.key
    chmod 600 etc/pms/node.key
    echo -e "   \${GREEN}✅ Generated node.key\${NC}"
fi

# TLS with SANs for Docker internal hostnames
if [ ! -f secrets/tls/cert.pem ]; then
    echo -e "   \${YELLOW}🔐 Generating TLS certificates with Docker SANs...\${NC}"
    openssl req -x509 -newkey rsa:4096 -keyout secrets/tls/key.pem \
        -out secrets/tls/cert.pem -days 365 -nodes \
        -subj "/CN=pms-node" \
        -addext "subjectAltName=IP:127.0.0.1,DNS:localhost,DNS:pms-engine,DNS:pms-gateway" \
        2>/dev/null
    chmod 644 secrets/tls/*.pem
    echo -e "   \${GREEN}✅ TLS certificates generated (with SANs: pms-engine, pms-gateway)\${NC}"
fi

# Prometheus config
if [ ! -f etc/prometheus/prometheus.yml ]; then
    echo -e "   \${YELLOW}📊 Creating Prometheus config...\${NC}"
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
    echo -e "   \${GREEN}✅ Prometheus config created\${NC}"
fi

# Config TOML from template
if [ ! -f etc/config/config.prod.toml ]; then
    if [ ! -f etc/config/config.prod.template.toml ]; then
        echo -e "   \${RED}❌ ERROR: config.prod.template.toml not found!\${NC}"
        exit 1
    fi
    echo "   Generating etc/config/config.prod.toml from template..."
    cp etc/config/config.prod.template.toml etc/config/config.prod.toml

    # Inject admin token into config
    sed -i "s|REPLACE_WITH_YOUR_SECRET_TOKEN|\$ADMIN_TOKEN|g" etc/config/config.prod.toml
    echo -e "   \${GREEN}✅ Admin token injected into config.prod.toml\${NC}"

    # If not initializing coordinator, comment out admin_wallet_file
    if [ "\$DO_INIT_COORD" != "true" ]; then
         echo "   Adapting config for non-coordinator node..."
         sed -i 's|admin_wallet_file =|# admin_wallet_file =|g' etc/config/config.prod.toml
    fi
fi

# Always update admin token in existing config (in case token changed between deploys)
sed -i "s|^admin_api_token = .*|admin_api_token = \"\$ADMIN_TOKEN\"|g" etc/config/config.prod.toml

# --- Caddyfile ---
echo -e "\${YELLOW}📝 Generating Caddyfile.prod for \$DOMAIN_NAME...\${NC}"
rm -rf Caddyfile.prod

# Determine TLS directive (IP = internal, Domain = Let's Encrypt)
TLS_DIRECTIVE=""
if [[ "\$DOMAIN_NAME" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+\$ ]]; then
    TLS_DIRECTIVE="tls internal"
else
    TLS_DIRECTIVE="tls admin@\$DOMAIN_NAME"
fi

cat > Caddyfile.prod << CADDY_EOF
{
    email admin@pms-network.com
}

\$DOMAIN_NAME, www.\$DOMAIN_NAME {
    \$TLS_DIRECTIVE
    reverse_proxy https://pms-gateway:8443 {
        transport http {
            tls
            tls_insecure_skip_verify
        }
    }
}
CADDY_EOF
echo -e "   \${GREEN}✅ Caddyfile.prod generated (→ pms-gateway:8443)\${NC}"

# --- Pre-Deployment Checklist ---
echo ""
echo -e "\${YELLOW}📂 Checking critical files...\${NC}"
echo "==================================================="
printf "%-40s %s\n" "File" "Status"
echo "---------------------------------------------------"

MISSING_CRITICAL=false

check_file() {
    local file=\$1
    local is_critical=\$2
    if [ -f "\$file" ]; then
        printf "%-40s \${GREEN}[✅ OK]\${NC}\n" "\$file"
    else
        printf "%-40s \${RED}[❌ MISSING]\${NC}\n" "\$file"
        if [ "\$is_critical" = "true" ]; then
            MISSING_CRITICAL=true
        fi
    fi
}

check_file "docker-compose.yml" "true"
check_file "Dockerfile" "true"
check_file "Dockerfile.gateway" "true"
check_file "etc/config/config.prod.toml" "true"
check_file "etc/pms/node.key" "true"
check_file "secrets/tls/cert.pem" "true"
check_file "Caddyfile.prod" "true"
check_file "etc/prometheus/prometheus.yml" "true"
check_file "pms-dashboard/package.json" "true"

# Treasury is critical only if we are NOT about to init/gen it
if [ "\$DO_INIT_COORD" = "true" ]; then
    check_file "etc/pms/treasury-wallets.json" "false"
else
    check_file "etc/pms/treasury-wallets.json" "true"
fi

echo "==================================================="

if [ "\$MISSING_CRITICAL" = "true" ]; then
    echo -e "\${RED}⚠️  CRITICAL FILES MISSING! Stopping deployment.\${NC}"
    echo "   Please fix the missing files or ensure setup ran correctly."
    exit 1
else
    echo -e "\${GREEN}✅ All critical files present.\${NC}"
fi

echo ""

# --- Action 2: Build & Start ---
if [ "\$DO_BUILD" = "true" ]; then

    echo -e "\${YELLOW}🐳 Rebuilding and Starting Containers...\${NC}"

    # Nettoyage si demandé
    if [ "\$DO_CLEAN_RESET" = "true" ]; then
        echo -e "\${RED}🧹 Cleaning ALL data (down -v)...\${NC}"
        docker compose -f docker-compose.yml down -v --remove-orphans 2>/dev/null || true
    else
        docker compose -f docker-compose.yml down --remove-orphans 2>/dev/null || true
    fi

    # Remove old containers if name changed
    docker rm -f pms-node pms-engine pms-gateway pms-caddy pms-prometheus 2>/dev/null || true

    # Build Engine + Gateway
    echo -e "\${YELLOW}🏗️  Building Engine image...\${NC}"
    docker compose -f docker-compose.yml build pms-engine
    echo -e "\${YELLOW}🏗️  Building Gateway image (with dashboard)...\${NC}"
    docker compose -f docker-compose.yml build pms-gateway

    # --- Coordinator Init (Pre-Start) ---
    if [ "\$DO_INIT_COORD" = "true" ]; then
        echo -e "\${YELLOW}👑 Initializing Coordinator & Treasury (Pre-Start)...\${NC}"

        # Fix permissions for Docker write access
        echo "   Fixing permissions for Docker write access..."
        chmod 777 etc/pms
        chmod 777 etc/config

        # 1. Gen Coordinator
        docker compose -f docker-compose.yml run --rm --entrypoint /bin/bash pms-engine -c \
            "/usr/local/bin/tools-cli gen-coordinator \
            /home/pms/config/pms/coordinator.key \
            /home/pms/config/pms/coordinator.json \
            /home/pms/config/config.prod.toml --force"

        # 2. Gen Treasury
        echo -e "\${YELLOW}🏦 Generating Treasury Wallet...\${NC}"
        docker compose -f docker-compose.yml run --rm --entrypoint /bin/bash pms-engine -c \
            "/usr/local/bin/tools-cli treasury-generate 1 \
            /home/pms/config/pms/treasury-keys \
            /home/pms/config/pms/treasury-wallets.json"

        # 3. Sign Treasury
        docker compose -f docker-compose.yml run --rm --entrypoint /bin/bash pms-engine -c \
            "/usr/local/bin/tools-cli treasury-sign \
            /home/pms/config/pms/coordinator.key \
            /home/pms/config/pms/treasury-wallets.json"

        echo -e "\${GREEN}✅ Coordinator Init Complete. Files generated in ./etc/pms/\${NC}"

        # Restore permissions
        chmod 755 etc/pms
        chmod 755 etc/config

        # Verification: Check if config was actually updated
        echo -e "\${YELLOW}🔍 Verifying Config Update...\${NC}"
        if grep -q 'coordinator_public_key = ""' etc/config/config.prod.toml; then
             echo -e "\${RED}❌ ERROR: Config file was NOT updated with new coordinator key!\${NC}"
             exit 1
        else
             echo -e "\${GREEN}✅ Config updated successfully.\${NC}"
        fi
    fi

    # Export admin token for docker-compose
    export PMS_ADMIN_TOKEN="\$ADMIN_TOKEN"

    # Start all services
    docker compose -f docker-compose.yml up -d --force-recreate

    echo -e "\${GREEN}✅ Containers started.\${NC}"

    # Wait for Engine health (max 30s)
    echo "⏳ Waiting for Engine to be healthy..."
    for i in {1..30}; do
        if docker exec pms-engine bash -c 'timeout 2 bash -c "</dev/tcp/127.0.0.1/8080"' 2>/dev/null; then
            echo -e "\${GREEN}✅ Engine is UP (internal)\${NC}"
            break
        fi
        if [ \$i -eq 30 ]; then
            echo -e "\${RED}❌ Engine failed to start! Dumping logs:\${NC}"
            docker compose logs --tail 50 pms-engine
            exit 1
        fi
        echo -n "."
        sleep 1
    done
    echo ""

    # Wait for Gateway health (max 30s)
    echo "⏳ Waiting for Gateway to be healthy..."
    for i in {1..30}; do
        if docker exec pms-gateway bash -c 'timeout 2 bash -c "</dev/tcp/127.0.0.1/8443"' 2>/dev/null; then
            echo -e "\${GREEN}✅ Gateway is UP\${NC}"
            break
        fi
        if [ \$i -eq 30 ]; then
            echo -e "\${RED}❌ Gateway failed to start! Dumping logs:\${NC}"
            docker compose logs --tail 50 pms-gateway
            exit 1
        fi
        echo -n "."
        sleep 1
    done
    echo ""

    # Show final status
    echo -e "\${YELLOW}📊 Service Status:\${NC}"
    docker compose ps

else
    echo "   Skipping Build & Restart."
fi

EOFREMOTE_MAIN

# -----------------------------------------------------------------------------
# 5. Secure Export (User Request)
# -----------------------------------------------------------------------------
echo ""
if ask_yes_no "   ❓ Download Secure Backup (keys, wallets) locally?" "Y"; then
    echo -e "${YELLOW}💾 Generating Secure Verification Export...${NC}"

    BACKUP_DIR="backups"
    DEFAULT_FILENAME="pms-deployment-$(date +%Y%m%d-%H%M%S).json"
    BACKUP_FILE=""

    # macOS Native "Save As" Dialog
    if command -v osascript &>/dev/null; then
        echo "   Requesting save location via dialog..."
        BACKUP_FILE=$(osascript -e "set fileName to choose file name with prompt \"Enregistrer la sauvegarde PMS :\" default name \"$DEFAULT_FILENAME\" default location (path to desktop folder)" -e "POSIX path of fileName" 2>/dev/null)
    fi

    # Fallback if user cancelled dialog or not on macOS
    if [ -z "$BACKUP_FILE" ]; then
        mkdir -p "$BACKUP_DIR"
        BACKUP_FILE="$BACKUP_DIR/$DEFAULT_FILENAME"
        if command -v osascript &>/dev/null; then
            echo "   ⚠️  Dialog cancelled. Defaulting to: $BACKUP_FILE"
        fi
    fi

    # Retrieve remote sensitive files content safely using SCP
    echo "   Fetching remote secrets via SCP..."

    TMP_DIR=$(mktemp -d)

    scp -q $VPS_USER@$VPS_IP:/opt/pms/etc/pms/coordinator.json "$TMP_DIR/coordinator.json" 2>/dev/null || echo "{}" > "$TMP_DIR/coordinator.json"
    scp -q $VPS_USER@$VPS_IP:/opt/pms/etc/pms/coordinator.key "$TMP_DIR/coordinator.key" 2>/dev/null || touch "$TMP_DIR/coordinator.key"
    scp -q $VPS_USER@$VPS_IP:/opt/pms/etc/pms/treasury-wallets.json "$TMP_DIR/treasury-wallets.json" 2>/dev/null || echo "[]" > "$TMP_DIR/treasury-wallets.json"

    mkdir -p "$TMP_DIR/treasury-keys"
    scp -q -r $VPS_USER@$VPS_IP:/opt/pms/etc/pms/treasury-keys/* "$TMP_DIR/treasury-keys/" 2>/dev/null || true

    # Generate final JSON locally
    if command -v python3 &>/dev/null; then
      python3 -c "
import json, os, sys, glob

try:
    def read_file(path, default):
        if not os.path.exists(path): return default
        with open(path, 'r') as f:
            content = f.read().strip()
            return content if content else default

    coord_json_raw = read_file('$TMP_DIR/coordinator.json', '{}')
    coord_key_raw = read_file('$TMP_DIR/coordinator.key', '')
    treasury_json_raw = read_file('$TMP_DIR/treasury-wallets.json', '[]')

    try:
        coord_wallet = json.loads(coord_json_raw)
    except:
        coord_wallet = {}

    try:
        treasury_wallets = json.loads(treasury_json_raw)
    except:
        treasury_wallets = []

    treasury_keys = []
    key_files = glob.glob('$TMP_DIR/treasury-keys/*.json')
    for kf in key_files:
        try:
            with open(kf, 'r') as f:
                key_data = json.load(f)
                treasury_keys.append(key_data)
        except:
            pass

    data = {
        'deployment_info': {
            'domain': '$DOMAIN_NAME',
            'admin_token': '$ADMIN_TOKEN',
            'vps_ip': '$VPS_IP',
            'user': '$VPS_USER'
        },
        'coordinator': {
            'wallet': coord_wallet,
            'private_key_hex': coord_key_raw
        },
        'treasury_list_public': treasury_wallets,
        'treasury_opt_keys_full': treasury_keys
    }
    print(json.dumps(data, indent=2))
except Exception as e:
    print(f'Error creating JSON: {e}', file=sys.stderr)
    sys.exit(1)
" > "$BACKUP_FILE"
    else
      cat > "$BACKUP_FILE" << EOF
{
  "COORD_NOTE": "Python missing, raw dump",
  "coordinator_wallet": $(cat "$TMP_DIR/coordinator.json"),
  "coordinator_key": "$(cat "$TMP_DIR/coordinator.key")",
  "treasury": $(cat "$TMP_DIR/treasury-wallets.json")
}
EOF
    fi

    # Cleanup temp
    rm -rf "$TMP_DIR"

    echo -e "   ${GREEN}✅ Secure Backup Saved: $BACKUP_FILE${NC}"
    echo -e "   ${RED}⚠️  KEEP THIS FILE SECRET! IT CONTAINS PRIVATE KEYS!${NC}"
fi

# -----------------------------------------------------------------------------
# 6. Connectivity Check
# -----------------------------------------------------------------------------
echo -e "${YELLOW}🔍 Verifying external connectivity...${NC}"
echo "   Request: GET https://$DOMAIN_NAME/livez"

if HTTP_STATUS=$(curl -sk -o /dev/null -w "%{http_code}" "https://$DOMAIN_NAME/livez"); then
    if [ "$HTTP_STATUS" == "200" ]; then
        echo -e "   Status:  ${GREEN}$HTTP_STATUS OK${NC}"
        echo -e "   ${GREEN}✅ Deployment Successful & Reachable!${NC}"
    else
        echo -e "   Status:  ${RED}$HTTP_STATUS${NC}"
        echo -e "   ${YELLOW}⚠️  Server reachable but returned error code.${NC}"
    fi
else
    echo -e "   ${RED}❌ Connection Failed.${NC} (DNS might be propagating or Firewall blocked)"
fi

echo ""
echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${GREEN}   PMS Deployment Complete (Engine + Gateway + Caddy)${NC}"
echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
echo ""
echo "   Services:"
echo "   - Public:      https://$DOMAIN_NAME (via Caddy Let's Encrypt)"
echo "   - Dashboard:   https://$DOMAIN_NAME/dashboard/"
echo "   - Prometheus:  http://VPS_IP:9091 (localhost only)"
echo ""
