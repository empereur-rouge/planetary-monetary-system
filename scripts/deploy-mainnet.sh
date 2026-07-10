#!/bin/bash
# =============================================================================
# Script de deploiement PMS Mainnet (Engine + Gateway + Caddy + Observability)
# =============================================================================
# Usage: ./deploy-mainnet.sh <VPS_IP> [USER] [SSH_KEY]
#        IMAGE_VERSION=v0.7.21 ./deploy-mainnet.sh <VPS_IP> pms ~/.ssh/pms_vps
#        ./deploy-mainnet.sh --yes <VPS_IP> [USER] [SSH_KEY]
#
# Options:
#   --yes, -y              Non-interactive mode: auto Y to build, N to clean reset,
#                          N to init coordinator, Y to backup. Réutilise l'admin
#                          token existant (ou le génère). Pour CI/CD et déploiement IA.
#
# Env vars:
#   IMAGE_VERSION          Tag semver pour les images Docker (défaut: v0.7.21).
#                          Bump à chaque release. Tag immutable = rollback simple
#                          (`IMAGE_VERSION=v0.7.20 ./scripts/upgrade-mainnet.sh`).
#
# SSH key auth recommandée :
#   ssh-keygen -t ed25519 -f ~/.ssh/pms_vps
#   ssh-copy-id -i ~/.ssh/pms_vps pms@<VPS_IP>
#   ./deploy-mainnet.sh <VPS_IP> pms ~/.ssh/pms_vps
#
# Architecture:
#   Internet -> Caddy (80/443, Let's Encrypt pms-network.com)
#                -> Gateway (8443, self-signed TLS interne)
#                    -> Engine (8080, HTTPS self-signed TLS interne)
#              Prometheus + Alertmanager + cAdvisor (loopback only)
#
#   PAS DE SIMULATEUR — vrais utilisateurs uniquement.
#
# Différences vs deploy-testnet.sh:
#   - Domaine: pms-network.com (root) au lieu de testnet.pms-network.com
#   - Image tags: semver via IMAGE_VERSION env (vs `:testnet` mutable)
#   - Pas de build/transfer/start du simulateur
#   - Backup paths: backups/mainnet/ + Misc/pms-key/pms-mainnet-*.json
#   - Container suffix: -mainnet
#   - Compose file: docker-compose.mainnet.yml
#   - Config file: etc/config/config.mainnet.toml
#
# Build local cross-compile linux/amd64, transfert via scp + docker load.
# Pas de build sur le VPS.

set -e

# -----------------------------------------------------------------------------
# Args parsing
# -----------------------------------------------------------------------------
AUTO_YES=false
POSITIONAL_ARGS=()
for arg in "$@"; do
    case "$arg" in
        --yes|-y) AUTO_YES=true ;;
        *) POSITIONAL_ARGS+=("$arg") ;;
    esac
done

VPS_IP="${POSITIONAL_ARGS[0]:-}"
VPS_USER="${POSITIONAL_ARGS[1]:-pms}"
SSH_KEY="${POSITIONAL_ARGS[2]:-}"

SSH_OPTS="-o ServerAliveInterval=30 -o ServerAliveCountMax=5 -o ConnectTimeout=10"
if [ -n "$SSH_KEY" ]; then
    SSH_OPTS="$SSH_OPTS -i $SSH_KEY"
fi

# Image tag semver. Défaut v0.7.21 ; bump avec chaque release.
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
    echo "Usage: $0 [--yes|-y] <VPS_IP> [USER] [SSH_KEY]"
    echo "       IMAGE_VERSION=v0.7.21 $0 87.106.x.y pms ~/.ssh/pms_vps"
    echo ""
    echo "Env IMAGE_VERSION = tag semver Docker (défaut: v0.7.21)"
    exit 1
fi

echo -e "${CYAN}===============================================${NC}"
echo -e "${CYAN}   PMS Mainnet Deployment (Local Build)${NC}"
echo -e "${CYAN}===============================================${NC}"
echo "   Target:        $VPS_USER@$VPS_IP"
echo "   Domain:        $DOMAIN_NAME"
echo "   Image version: $IMAGE_VERSION"
echo "   Stack:         Engine + Gateway + Caddy + Prometheus + Alertmanager + cAdvisor"
echo -e "   Build:         ${BOLD}Local (cross-compile linux/amd64)${NC}"
if [ "$AUTO_YES" = "true" ]; then
    echo -e "   Mode:          ${YELLOW}NON-INTERACTIVE (--yes)${NC}"
fi
echo ""

# -----------------------------------------------------------------------------
# Utilities
# -----------------------------------------------------------------------------
ask_yes_no() {
    local prompt="$1"
    local default="$2"
    local reply

    if [ "$AUTO_YES" = "true" ]; then
        if [ "$default" = "Y" ]; then
            echo "$prompt → auto: Y"
            return 0
        else
            echo "$prompt → auto: N"
            return 1
        fi
    fi

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

if [ "$AUTO_YES" = "true" ]; then
    ADMIN_TOKEN=""
    LATEST_BACKUP=$(ls -t "/Volumes/Crutial X9 - Macbook Erwan/Misc/pms-key/pms-mainnet-"*.json 2>/dev/null | head -1)
    if [ -z "$LATEST_BACKUP" ]; then
        LATEST_BACKUP=$(ls -t backups/mainnet/pms-mainnet-*.json 2>/dev/null | head -1)
    fi
    if [ -n "$LATEST_BACKUP" ]; then
        ADMIN_TOKEN=$(python3 -c "import json; print(json.load(open('$LATEST_BACKUP')).get('deployment',{}).get('admin_token',''))" 2>/dev/null || echo "")
    fi
    if [ -z "$ADMIN_TOKEN" ]; then
        ADMIN_TOKEN=$(openssl rand -hex 32)
        echo -e "   Auto-generated: ${GREEN}${ADMIN_TOKEN:0:20}...${NC}"
    else
        echo -e "   Reused from backup: ${GREEN}${ADMIN_TOKEN:0:20}...${NC}"
    fi
else
    read -p "   Enter ADMIN_TOKEN (leave empty to generate random): " ADMIN_TOKEN
    if [ -z "$ADMIN_TOKEN" ]; then
        ADMIN_TOKEN=$(openssl rand -hex 32)
        echo -e "   Generated: ${GREEN}$ADMIN_TOKEN${NC}"
    fi
fi

# -----------------------------------------------------------------------------
# 2. Action selection
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}[2/7] Select Actions${NC}"

DO_BUILD=false
if ask_yes_no "   Build & deploy Docker images?" "Y"; then
    DO_BUILD=true
fi

DO_CLEAN_RESET=false
if ask_yes_no "   [DANGER] Clean Reset (delete ALL mainnet data/volumes)?" "N"; then
    DO_CLEAN_RESET=true
fi

DO_INIT_COORD=false
if ask_yes_no "   Initialize Coordinator (required on first deploy)?" "N"; then
    DO_INIT_COORD=true
fi

# -----------------------------------------------------------------------------
# 3. Local Build
# -----------------------------------------------------------------------------
if [ "$DO_BUILD" = "true" ]; then
    echo ""
    echo -e "${YELLOW}[3/7] Building images locally (linux/amd64)...${NC}"
    echo -e "   ${BOLD}This runs on YOUR machine, not the VPS.${NC}"
    echo -e "   Tag: ${BOLD}$IMAGE_VERSION${NC}"

    mkdir -p "$LOCAL_IMG_DIR"

    if ! docker buildx inspect pms-builder >/dev/null 2>&1; then
        echo -e "   Creating buildx builder..."
        docker buildx create --name pms-builder --use >/dev/null 2>&1
    else
        docker buildx use pms-builder >/dev/null 2>&1
    fi

    # Build Engine
    echo ""
    echo -e "   ${CYAN}[1/2] Building Engine (pms-node:$IMAGE_VERSION)...${NC}"
    docker buildx build \
        --platform linux/amd64 \
        -t "pms-node:$IMAGE_VERSION" \
        --output "type=docker,dest=$LOCAL_IMG_DIR/pms-node.tar" \
        .
    echo -e "   ${GREEN}Engine built.${NC}"

    # Build Gateway
    echo ""
    echo -e "   ${CYAN}[2/2] Building Gateway (pms-gateway:$IMAGE_VERSION)...${NC}"
    docker buildx build \
        --platform linux/amd64 \
        -t "pms-gateway:$IMAGE_VERSION" \
        -f Dockerfile.gateway \
        --output "type=docker,dest=$LOCAL_IMG_DIR/pms-gateway.tar" \
        .
    echo -e "   ${GREEN}Gateway built.${NC}"

    # PAS de simulator build en mainnet.

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

if [ "$DO_BUILD" = "true" ]; then
    echo -e "   Stopping services before upload (prevents disk fill during transfer)..."
    ssh -T $SSH_OPTS "$VPS_USER@$VPS_IP" "
        cd $REMOTE_DIR 2>/dev/null || true
        PMS_ADMIN_TOKEN='placeholder' docker compose -f $COMPOSE_FILE down --remove-orphans 2>/dev/null || true
        docker rm -f pms-engine-mainnet pms-gateway-mainnet pms-caddy-mainnet pms-prometheus-mainnet pms-alertmanager-mainnet pms-cadvisor-mainnet 2>/dev/null || true
        docker image prune -f 2>/dev/null || true
    "
    echo -e "   ${GREEN}Services stopped, disk cleaned.${NC}"
fi

ssh -T $SSH_OPTS "$VPS_USER@$VPS_IP" "mkdir -p $REMOTE_DIR/etc/config $REMOTE_DIR/etc/pms $REMOTE_DIR/secrets/tls $REMOTE_DIR/etc/prometheus $REMOTE_DIR/etc/alertmanager"

echo -e "   Uploading config files..."
scp -q $SSH_OPTS "$COMPOSE_FILE" "$VPS_USER@$VPS_IP:$REMOTE_DIR/$COMPOSE_FILE"

# Preserve VPS-specific config values if not re-initializing coordinator
if [ "$DO_INIT_COORD" = "false" ]; then
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
fi

scp -q $SSH_OPTS "$CONFIG_FILE" "$VPS_USER@$VPS_IP:$REMOTE_DIR/$CONFIG_FILE"

# Restore VPS-specific config values via heredocs (TOML quotes survive)
if [ "$DO_INIT_COORD" = "false" ] && [ -n "$SAVED_COORD_KEY" ]; then
    echo -e "   Preserving VPS config values..."
    ssh -T -q $SSH_OPTS "$VPS_USER@$VPS_IP" << RESTORE_KEYS_EOF
sed -i 's|^coordinator_public_key = .*|$SAVED_COORD_KEY|' $REMOTE_DIR/$CONFIG_FILE
sed -i 's|^coordinator_x25519_public_key = .*|$SAVED_X25519_KEY|' $REMOTE_DIR/$CONFIG_FILE
RESTORE_KEYS_EOF
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
    echo -e "   ${GREEN}VPS config values preserved.${NC}"
fi

# Prometheus + alertmanager configs
[ -f etc/prometheus/prometheus.mainnet.yml ] && \
    scp -q $SSH_OPTS etc/prometheus/prometheus.mainnet.yml "$VPS_USER@$VPS_IP:$REMOTE_DIR/etc/prometheus/prometheus.mainnet.yml"
[ -f etc/prometheus/alerting_rules.yml ] && \
    scp -q $SSH_OPTS etc/prometheus/alerting_rules.yml "$VPS_USER@$VPS_IP:$REMOTE_DIR/etc/prometheus/alerting_rules.yml"
[ -f etc/alertmanager/alertmanager.mainnet.yml ] && \
    scp -q $SSH_OPTS etc/alertmanager/alertmanager.mainnet.yml "$VPS_USER@$VPS_IP:$REMOTE_DIR/etc/alertmanager/alertmanager.mainnet.yml"

# Boot-resiliency (idem testnet) : compose.yaml symlink + legacy disable
ssh -T -q $SSH_OPTS "$VPS_USER@$VPS_IP" "cd $REMOTE_DIR && \
    ln -sf docker-compose.mainnet.yml compose.yaml && \
    if [ -f docker-compose.yml ] && [ ! -L docker-compose.yml ]; then \
        mv docker-compose.yml docker-compose.legacy-prod.yml.disabled; \
        echo '   Disabled legacy docker-compose.yml'; \
    fi"
echo -e "   ${GREEN}Config files uploaded.${NC}"

# Transfer Docker images
if [ "$DO_BUILD" = "true" ]; then
    echo -e "   Uploading Docker images to VPS (this may take a few minutes)..."
    ssh -T $SSH_OPTS "$VPS_USER@$VPS_IP" "mkdir -p /tmp/pms-images-mainnet"

    for img in pms-node pms-gateway; do
        SIZE=$(du -sh "$LOCAL_IMG_DIR/$img.tar.gz" | awk '{print $1}')
        echo -e "   Uploading $img ($SIZE)..."
        scp $SSH_OPTS "$LOCAL_IMG_DIR/$img.tar.gz" "$VPS_USER@$VPS_IP:/tmp/pms-images-mainnet/"
    done

    echo -e "   ${GREEN}All images uploaded.${NC}"

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
IMAGE_VERSION="$IMAGE_VERSION"
DO_BUILD="$DO_BUILD"
DO_CLEAN_RESET="$DO_CLEAN_RESET"
DO_INIT_COORD="$DO_INIT_COORD"

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

cd $REMOTE_DIR
export IMAGE_VERSION="\$IMAGE_VERSION"

# --- Load Docker images ---
if [ "\$DO_BUILD" = "true" ]; then
    echo -e "\${YELLOW}   Removing old images before load (prevents containerd bloat)...\${NC}"
    for img_tag in "pms-node:\$IMAGE_VERSION" "pms-gateway:\$IMAGE_VERSION"; do
        docker rmi "\$img_tag" 2>/dev/null || true
    done
    docker image prune -f 2>/dev/null || true

    echo -e "\${YELLOW}   Loading Docker images...\${NC}"
    for img in /tmp/pms-images-mainnet/*.tar.gz; do
        NAME=\$(basename "\$img" .tar.gz)
        echo -n "   Loading \$NAME... "
        gunzip -c "\$img" | docker load 2>&1 | tail -1
    done
    rm -rf /tmp/pms-images-mainnet
    echo -e "\${GREEN}   All images loaded.\${NC}"

    docker image prune -f 2>/dev/null || true
fi

# --- Setup secrets ---
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
    echo -e "   \${YELLOW}Generating internal TLS certificates (engine↔gateway)...\${NC}"
    openssl req -x509 -newkey rsa:4096 -keyout secrets/tls/key.pem \
        -out secrets/tls/cert.pem -days 365 -nodes \
        -subj "/CN=pms-mainnet" \
        -addext "subjectAltName=IP:127.0.0.1,DNS:localhost,DNS:pms-engine,DNS:pms-gateway" \
        2>/dev/null
    chmod 644 secrets/tls/*.pem
    echo -e "   \${GREEN}TLS certificates generated\${NC}"
fi

# --- Caddyfile mainnet (Let's Encrypt sur pms-network.com) ---
cat > Caddyfile.mainnet << CADDY_EOF
{
    email admin@pms-network.com
}

\$DOMAIN_NAME {
    tls admin@pms-network.com
    reverse_proxy https://pms-gateway:8443 {
        # SECURITE (v0.30.2) : ECRASE X-Forwarded-For avec l'IP reelle du peer.
        # Sans cette ligne Caddy APPEND a un XFF potentiellement falsifie par le
        # client ; le rate limiter per-IP (gateway ET engine, SmartIpKeyExtractor
        # lit la valeur la plus a gauche) devient alors contournable par rotation
        # de XFF et empoisonnable (429 cible sur l'IP d'une victime). Le gateway
        # forwarde ce XFF a l'engine, donc l'ecrasement DOIT etre ici. Cf. revue
        # securite DoS v0.30.2.
        header_up X-Forwarded-For {remote_host}
        transport http {
            tls
            tls_insecure_skip_verify
        }
    }
}
CADDY_EOF
echo -e "   \${GREEN}Caddyfile.mainnet generated\${NC}"

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
check_file "Caddyfile.mainnet" "true"
check_file "etc/prometheus/prometheus.mainnet.yml" "true"
check_file "etc/prometheus/alerting_rules.yml" "true"
check_file "etc/alertmanager/alertmanager.mainnet.yml" "true"

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

    if [ "\$DO_CLEAN_RESET" = "true" ]; then
        echo -e "\${RED}   Cleaning ALL mainnet data (volumes)...\${NC}"
        PMS_ADMIN_TOKEN="\$ADMIN_TOKEN" docker compose -f \$COMPOSE_FILE down -v --remove-orphans 2>/dev/null || true
    fi

    # --- Coordinator Init ---
    if [ "\$DO_INIT_COORD" = "true" ]; then
        echo ""
        echo -e "\${YELLOW}   Initializing Coordinator...\${NC}"
        echo -e "   \${BOLD}IMPORTANT: cette clé est UNIQUE au mainnet.\${NC}"
        echo -e "   \${BOLD}Ne réutilise jamais la clé testnet ici.\${NC}"

        chmod 755 etc/pms
        chmod 755 etc/config

        # 1. Gen Coordinator
        PMS_ADMIN_TOKEN="\$ADMIN_TOKEN" docker compose -f \$COMPOSE_FILE run --rm --entrypoint /bin/bash pms-engine -c \
            "/usr/local/bin/tools-cli gen-coordinator \
            /home/pms/config/pms/coordinator.key \
            /home/pms/config/pms/coordinator.json \
            /home/pms/config/config.mainnet.toml --force"

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
        COORD_PUBKEY=\$(python3 -c "import json; print(json.load(open('etc/pms/coordinator.json'))['public_key'])" 2>/dev/null || echo "")

        if [ -n "\$COORD_ADDR" ]; then
            sed -i "s|^wallet_addresses = \\[\\]|wallet_addresses = [\"\$COORD_ADDR\"]|" \$CONFIG_FILE
            echo -e "   \${GREEN}admin.wallet_addresses = [\$COORD_ADDR]\${NC}"
        fi

        if [ -n "\$COORD_PUBKEY" ]; then
            if grep -q '^signer_pubkeys' \$CONFIG_FILE; then
                sed -i "s|^signer_pubkeys = .*|signer_pubkeys = [\"\$COORD_PUBKEY\"]|" \$CONFIG_FILE
            else
                sed -i "/^\\[admin\\]/a signer_pubkeys = [\"\$COORD_PUBKEY\"]" \$CONFIG_FILE
            fi
            echo -e "   \${GREEN}admin.signer_pubkeys = [\${COORD_PUBKEY:0:20}...]\${NC}"
        fi

        if [ -n "\$TREASURY_ADDR" ]; then
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

    # Start core services
    echo ""
    echo -e "\${YELLOW}   Starting core services (Engine, Gateway, Caddy, Prometheus, Alertmanager, cAdvisor)...\${NC}"
    export PMS_ADMIN_TOKEN="\$ADMIN_TOKEN"

    # Write the admin token where Prometheus can read it
    mkdir -p secrets
    printf '%s' "\$ADMIN_TOKEN" > secrets/prometheus_admin_token
    chmod 644 secrets/prometheus_admin_token
    echo -e "   \${GREEN}Prometheus admin token written\${NC}"

    # Telegram bot token placeholder (operator must replace with real
    # token to activate paging — same value as testnet, copy from
    # backup or testnet VPS).
    if [ ! -s secrets/telegram_bot_token ]; then
        printf '%s' 'PLACEHOLDER_PASTE_BOT_TOKEN_FROM_BOTFATHER_HERE' > secrets/telegram_bot_token
        echo -e "   \${YELLOW}Telegram bot token placeholder created — paging is OFF until you replace it.\${NC}"
        echo -e "   \${YELLOW}Action: copy the bot token from the testnet VPS (same bot, different channel).\${NC}"
    fi
    chmod 644 secrets/telegram_bot_token

    # Clean stale Compose state
    docker compose -f \$COMPOSE_FILE rm -f -s 2>/dev/null || true

    # Start services
    docker compose -f \$COMPOSE_FILE up -d --force-recreate --remove-orphans 2>&1 || true

    # Verify each core service is running
    for _svc in pms-engine:pms-engine-mainnet pms-gateway:pms-gateway-mainnet caddy:pms-caddy-mainnet prometheus:pms-prometheus-mainnet alertmanager:pms-alertmanager-mainnet cadvisor:pms-cadvisor-mainnet; do
        _compose_name=\${_svc%%:*}
        _container_name=\${_svc##*:}
        if ! docker ps --filter "name=\$_container_name" --filter "status=running" -q 2>/dev/null | grep -q .; then
            echo -e "\${YELLOW}   \$_container_name not running — retrying individually...\${NC}"
            docker compose -f \$COMPOSE_FILE up -d "\$_compose_name" 2>/dev/null || true
            sleep 2
        fi
    done

    echo -e "\${GREEN}   Core services started.\${NC}"

    # Wait for Engine
    echo "   Waiting for Engine..."
    for i in \$(seq 1 30); do
        if docker exec pms-engine-mainnet bash -c 'timeout 2 bash -c "</dev/tcp/127.0.0.1/8080"' 2>/dev/null; then
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
        if docker exec pms-gateway-mainnet bash -c 'timeout 2 bash -c "</dev/tcp/127.0.0.1/8443"' 2>/dev/null; then
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

    # SDK API Key
    SDK_API_KEY=""
    if [ -f etc/pms/sdk-api-key.json ]; then
        SDK_API_KEY=\$(python3 -c "import json; print(json.load(open('etc/pms/sdk-api-key.json')).get('key',''))" 2>/dev/null || echo "")
        if [ -n "\$SDK_API_KEY" ]; then
            echo -e "   \${GREEN}Reusing existing SDK API Key: \${SDK_API_KEY:0:20}...\${NC}"
        fi
    fi

    if [ -z "\$SDK_API_KEY" ]; then
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
        else
            echo -e "   \${RED}Could not create API key. Response: \${API_KEY_RESPONSE}\${NC}"
        fi
    fi

    docker image prune -f 2>/dev/null || true

    echo ""
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

    DEFAULT_FILENAME="pms-mainnet-$(date +%Y%m%d-%H%M%S).json"
    BACKUP_FILE=""

    if [ "$AUTO_YES" = "true" ]; then
        BACKUP_DIR="/Volumes/Crutial X9 - Macbook Erwan/Misc/pms-key"
        mkdir -p "$BACKUP_DIR"
        BACKUP_FILE="$BACKUP_DIR/$DEFAULT_FILENAME"
    elif command -v osascript &>/dev/null; then
        BACKUP_FILE=$(osascript -e "set fileName to choose file name with prompt \"Save PMS Mainnet Backup:\" default name \"$DEFAULT_FILENAME\" default location (path to desktop folder)" -e "POSIX path of fileName" 2>/dev/null)
    fi

    if [ -z "$BACKUP_FILE" ]; then
        BACKUP_DIR="backups/mainnet"
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
        'type': 'mainnet',
        'domain': '$DOMAIN_NAME',
        'image_version': '$IMAGE_VERSION',
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
    echo -e "   ${RED}KEEP THIS FILE SECRET — contains MAINNET private keys!${NC}"
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
        echo -e "   ${GREEN}$HTTP_STATUS OK — Mainnet is live!${NC}"
    else
        echo -e "   ${YELLOW}$HTTP_STATUS — Server reachable but returned error.${NC}"
    fi
else
    echo -e "   ${RED}Connection failed (DNS propagating or firewall).${NC}"
fi

# -----------------------------------------------------------------------------
# Final Recap
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
echo -e "${CYAN}   PMS Mainnet Deployment Complete${NC}"
echo -e "${CYAN}===============================================${NC}"
echo ""
echo "   Services:"
echo "   - Mainnet API:     https://$DOMAIN_NAME"
echo "   - Dashboard:       https://$DOMAIN_NAME/dashboard/"
echo "   - Prometheus:      http://localhost:9091 (VPS only)"
echo "   - Alertmanager:    http://localhost:9093 (VPS only)"
echo "   - cAdvisor:        http://localhost:8082 (VPS only)"
echo ""
echo -e "   ${BOLD}Image version:${NC}   $IMAGE_VERSION"
echo ""
echo -e "   ${BOLD}Admin Token:${NC}"
echo -e "   - Token:       ${RED}$ADMIN_TOKEN${NC}"
echo ""
if [ -n "$COORD_ADDRESS" ]; then
    echo -e "   ${BOLD}Coordinator Wallet (MAINNET):${NC}"
    echo -e "   - Address:     ${GREEN}$COORD_ADDRESS${NC}"
    echo -e "   - Private Key: ${RED}$COORD_PRIVKEY${NC}"
    echo -e "   - Public Key:  $COORD_PUBKEY"
    if [ -n "$COORD_MNEMONIC" ]; then
        echo -e "   - Mnemonic:    ${RED}$COORD_MNEMONIC${NC}"
    else
        echo -e "   - Mnemonic:    ${YELLOW}(not available — generated from raw key)${NC}"
    fi
    echo ""
    echo -e "   ${RED}KEEP PRIVATE KEY & MNEMONIC SECRET — MAINNET MEANS REAL VALUE!${NC}"
    echo ""
fi
echo "   Useful commands (on VPS):"
echo "   - Logs engine:      docker logs -f pms-engine-mainnet"
echo "   - Logs gateway:     docker logs -f pms-gateway-mainnet"
echo "   - Status:           docker compose -f $COMPOSE_FILE ps"
echo "   - Stop:             PMS_ADMIN_TOKEN=$ADMIN_TOKEN docker compose -f $COMPOSE_FILE down"
echo ""
echo -e "   ${YELLOW}Action required: configure mainnet Telegram channel chat_id in${NC}"
echo -e "   ${YELLOW}  etc/alertmanager/alertmanager.mainnet.yml then re-run upgrade-mainnet.sh${NC}"
echo -e "   ${YELLOW}  to push the change.${NC}"
echo ""
