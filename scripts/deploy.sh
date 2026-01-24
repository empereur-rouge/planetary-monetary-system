#!/bin/bash
# =============================================================================
# Script de déploiement PMS Interactif
# =============================================================================
# Usage: ./deploy.sh <VPS_IP> [USER]
#
# Ce script guide l'utilisateur à travers les étapes de déploiement:
# 1. Configuration (Admin Token & Domain)
# 2. Mise à jour (Git Pull)
# 3. Build & Restart (Docker)
# 4. Initialisation Coordinateur (Optionnel)

set -e

VPS_IP="${1:-}"
VPS_USER="${2:-root}"

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

# Hardcoded default domain if not provided as argument
# if [ -z "$DOMAIN_NAME" ]; then
#     DOMAIN_NAME="pms-network.com"
# fi

echo -e "${YELLOW}🚀 PMS Interactive Deployment Tool${NC}"
echo "   Target: $VPS_USER@$VPS_IP"
echo "   Domain: $DOMAIN_NAME"
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

# Pas de prompt pour le domaine car il est hardcodé (sauf si on veut confirmer)
# echo ""
# echo -e "${YELLOW}🌐 Domain Configuration${NC}"
# echo "   Using domain: $DOMAIN_NAME"

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
        # On reset les changements locaux pour permettre le pull
        # (Les fichiers de config seront ré-écrasés par le SCP juste après)
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
# 5. Execution: Phase 3 (Config & Deploy)
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}🔌 Connecting to VPS (Phase 2: Deploy)...${NC}"

ssh -T $VPS_USER@$VPS_IP << EOFREMOTE_MAIN
set -e

# Définition des variables sur le distant (Phase 2)
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
mkdir -p etc/pms secrets/tls

# node.key
if [ ! -f etc/pms/node.key ]; then
    openssl rand -hex 32 > etc/pms/node.key
    chmod 600 etc/pms/node.key
fi

# TLS
if [ ! -f secrets/tls/cert.pem ]; then
    openssl req -x509 -newkey rsa:4096 -keyout secrets/tls/key.pem \
        -out secrets/tls/cert.pem -days 365 -nodes \
        -subj "/CN=pms-node" 2>/dev/null
    chmod 644 secrets/tls/*.pem
fi

# Config TOML
# (Le token sera injecté directement dans le docker-compose)

# On assure que la config prod existe
if [ ! -f etc/config/config.prod.toml ]; then
    cp etc/config/config.prod.template.toml etc/config/config.prod.toml
    sed -i 's|/data/pms/rocks|/home/pms/data/rocks|g' etc/config/config.prod.toml
    sed -i 's|/etc/pms/tls|/home/pms/tls|g' etc/config/config.prod.toml
    sed -i 's|ca_pem =|# ca_pem =|g' etc/config/config.prod.toml
fi

# IMPORTANT: On utilise le docker-compose.yml du repo (OU celui uploadé par SCP)

# --- Caddyfile Check ---
# On s'assure que le Caddyfile existe (et que ce n'est pas un dossier résiduel de Docker)
echo -e "${YELLOW}📝 Generating Caddyfile.prod for $DOMAIN_NAME...${NC}"
rm -rf Caddyfile.prod

# Détermine si on doit utiliser SSL automatique ou interne
TLS_DIRECTIVE=""
# (Si c'est une IP, Caddy ne peut pas faire de cetr auto -> tls internal)
if [[ "$DOMAIN_NAME" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    TLS_DIRECTIVE="tls internal"
else
    # Vrai domaine -> Let's Encrypt automatique
    # On met une email admin par défaut
    TLS_DIRECTIVE="tls admin@$DOMAIN_NAME" 
fi

cat > Caddyfile.prod << EOF
{
    # Options globales
    email admin@pms-network.com
}

$DOMAIN_NAME, www.$DOMAIN_NAME, api.$DOMAIN_NAME {
    $TLS_DIRECTIVE
    reverse_proxy https://node1:8080 {
        transport http {
            tls
            tls_insecure_skip_verify
        }
    }
}
EOF


# --- Pre-Deployment Checklist --- (Executed on VPS)
# Check files AFTER they are supposed to be generated by Setup
echo ""
echo -e "\${YELLOW}📂 Checking critical files... \${NC}"
echo "==================================================="
printf "%-40s %s\n" "File" "Status"
echo "---------------------------------------------------"

MISSING_CRITICAL=false

check_file() {
    local file=\$1
    local is_critical=\$2 # "true" or "false"
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
check_file "etc/config/config.prod.toml" "true"
check_file "etc/pms/node.key" "true"
check_file "secrets/tls/cert.pem" "true"
check_file "Caddyfile.prod" "true"

# Treasury is critical only if we are NOT about to init/gen it
if [ "\$DO_INIT_COORD" = "true" ]; then
    check_file "etc/pms/treasury-wallets.json" "false" # Will be generated
else
    check_file "etc/pms/treasury-wallets.json" "true" # Must exist already
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



    echo -e "${YELLOW}🐳 Rebuilding and Starting Containers...${NC}"
    
    # Nettoyage si demandé
    if [ "$DO_CLEAN_RESET" = "true" ]; then
        echo -e "${RED}🧹 Cleaning ALL data (down -v)...${NC}"
        docker compose -f docker-compose.yml down -v --remove-orphans 2>/dev/null || true
    else
        docker compose -f docker-compose.yml down --remove-orphans 2>/dev/null || true
    fi
    
    # Sécurité supplémentaire : On supprime explicitement le conteneur s'il traîne encore
    # (Cas de changement de nom de service ex: node -> node1 avec même container_name)
    docker rm -f pms-node 2>/dev/null || true
    
    # Build
    # Note: On build pms-node qui contient tools-cli
    docker compose -f docker-compose.yml build --no-cache
    
    # --- Action 2b: Init Coordinator (Pre-Start) ---
    if [ "\$DO_INIT_COORD" = "true" ]; then
        echo -e "\${YELLOW}👑 Initializing Coordinator & Treasury (Pre-Start)...\${NC}"
        
        # FIX PERMISSIONS: Ensure container user (pms) can write to mounted config and dir
        echo "   Fixing permissions for Docker write access..."
        chmod 777 etc/pms
        chmod 777 etc/config

        # Use 'docker compose run --rm' to generate files in mounted volumes without starting the full node service
        # 1. Gen Coordinator
        # ⚠️  ENTRYPOINT OVERRIDE: Use bash wrapper to ensure tools-cli runs
        docker compose -f docker-compose.yml run --rm --entrypoint /bin/bash node1 -c \
            "/usr/local/bin/tools-cli gen-coordinator \
            /home/pms/config/pms/coordinator.key \
            /home/pms/config/pms/coordinator.json \
            /home/pms/config/config.prod.toml --force"

        # 2. Gen Treasury (Path modified to /home/pms/config/pms/ to persist in ./etc/pms volume)
        echo -e "\${YELLOW}🏦 Generating Treasury Wallet...\${NC}"
        docker compose -f docker-compose.yml run --rm --entrypoint /bin/bash node1 -c \
            "/usr/local/bin/tools-cli treasury-generate 1 \
            /home/pms/config/pms/treasury-keys \
            /home/pms/config/pms/treasury-wallets.json"

        # 3. Sign Treasury
        docker compose -f docker-compose.yml run --rm --entrypoint /bin/bash node1 -c \
            "/usr/local/bin/tools-cli treasury-sign \
            /home/pms/config/pms/coordinator.key \
            /home/pms/config/pms/treasury-wallets.json"
            
        echo -e "\${GREEN}✅ Init Complete. Files generated in ./etc/pms/\${NC}"
        
        echo "MAGIC_JSON_START"
        cat etc/pms/coordinator.json
        echo "MAGIC_JSON_END"
        
        # Verification: Check if config was actually updated
        echo -e "\${YELLOW}🔍 Verifying Config Update...\${NC}"
        # We check specifically for the assignment line to avoid matching the other placeholder in authority_public_keys
        if grep -q 'coordinator_public_key = "03XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"' etc/config/config.prod.toml; then
             echo -e "\${RED}❌ ERROR: Config file was NOT updated with new coordinator key! (Still has placeholder)\${NC}"
             exit 1
        else
             echo -e "\${GREEN}✅ Config updated successfully.\${NC}"
        fi
    fi
    
    # Start (Force recreate to ensure config is picked up)
    docker compose -f docker-compose.yml up -d --force-recreate

    echo -e "${GREEN}✅ Containers started.${NC}"
    
    # Wait for health check (max 30s)
    echo "⏳ Waiting for health check..."
    for i in {1..30}; do
        if docker compose ps | grep -q "healthy"; then
            echo -e "${GREEN}✅ Health check passed!${NC}"
            break
        fi

        # Debug: Check if it crashed
        if ! docker compose ps | grep -q "Up"; then
             echo -e "${RED}❌ Container CRASHED! Dumping logs:${NC}"
             docker compose logs --tail 50 node1
             exit 1
        fi
        
        # Verbose progress
        echo -n "."
        sleep 1
    done
    echo ""

    # Final verification
    if docker compose ps | grep -q "unhealthy"; then
        echo -e "${RED}❌ Container is UNHEALTHY! Dumping logs:${NC}"
        docker compose logs --tail 50 node1
    fi

else
    echo "   Skipping Build & Restart."
fi




EOFREMOTE_MAIN

# -----------------------------------------------------------------------------
# 6. Secure Export (User Request)
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
        # AppleScript: choose file name
        # We use 'try' block to handle user cancel
        BACKUP_FILE=$(osascript -e "set fileName to choose file name with prompt \"Enregistrer la sauvegarde PMS :\" default name \"$DEFAULT_FILENAME\" default location (path to desktop folder)" -e "POSIX path of fileName" 2>/dev/null)
    fi

    # Fallback if user cancelled dialog or not on macOS
    if [ -z "$BACKUP_FILE" ]; then
        mkdir -p "$BACKUP_DIR"
        BACKUP_FILE="$BACKUP_DIR/$DEFAULT_FILENAME"
        if command -v osascript &>/dev/null; then
             # User hit cancel on the dialog, usually implies they don't want to save
             # But we default to backup dir just in case
            echo "   ⚠️  Dialog cancelled. Defaulting to: $BACKUP_FILE"
        fi
    fi

    # Retrieve remote sensitive files content safely
    # We print them specifically to capture them
    echo "   Fetching remote secrets..."

    REMOTE_DATA=$(ssh -T $VPS_USER@$VPS_IP << EOFREMOTE_EXPORT
      set -e
      cd /opt/pms
      echo "---START_COORD_JSON---"
      cat etc/pms/coordinator.json 2>/dev/null || echo "{}"
      echo "---END_COORD_JSON---"

      echo "---START_COORD_KEY---"
      cat etc/pms/coordinator.key 2>/dev/null || echo ""
      echo "---END_COORD_KEY---"

      echo "---START_TREASURY---"
      cat etc/pms/treasury-wallets.json 2>/dev/null || echo "[]"
      echo "---END_TREASURY---"
EOFREMOTE_EXPORT
    )

    # Extract content using bash string manipulation
    COORD_JSON=$(echo "$REMOTE_DATA" | sed -n '/---START_COORD_JSON---/,/---END_COORD_JSON---/p' | sed '1d;$d')
    COORD_KEY=$(echo "$REMOTE_DATA" | sed -n '/---START_COORD_KEY---/,/---END_COORD_KEY---/p' | sed '1d;$d')
    TREASURY_JSON=$(echo "$REMOTE_DATA" | sed -n '/---START_TREASURY---/,/---END_TREASURY---/p' | sed '1d;$d')

    # Generate final JSON locally
    # Using python3 for safe JSON formatting if available, otherwise fallback to simple string construction
    if command -v python3 &>/dev/null; then
      python3 -c "
import json, sys

data = {
    'deployment_info': {
        'domain': '$DOMAIN_NAME',
        'admin_token': '$ADMIN_TOKEN',
        'vps_ip': '$VPS_IP',
        'user': '$VPS_USER'
    },
    'coordinator': {
        'wallet': json.loads('''$COORD_JSON'''),
        'private_key_hex': '$COORD_KEY'.strip()
    },
    'treasury_wallets': json.loads('''$TREASURY_JSON''')
}
print(json.dumps(data, indent=2))
" > "$BACKUP_FILE"
    else
      # Fallback for basic environments
      cat > "$BACKUP_FILE" << EOF
{
  "deployment_info": {
     "domain": "$DOMAIN_NAME",
     "admin_token": "$ADMIN_TOKEN",
     "vps_ip": "$VPS_IP"
  },
  "coordinator": {
     "wallet": $COORD_JSON,
     "private_key_hex": "$COORD_KEY"
  },
  "treasury_wallets": $TREASURY_JSON
}
EOF
    fi

    echo -e "   ${GREEN}✅ Secure Backup Saved: $BACKUP_FILE${NC}"
    echo -e "   ${RED}⚠️  KEEP THIS FILE SECRET! IT CONTAINS PRIVATE KEYS!${NC}"
fi

# -----------------------------------------------------------------------------
# 7. Connectivity Check
# -----------------------------------------------------------------------------
echo -e "${YELLOW}🔍 Verifying external connectivity...${NC}"
echo "   Request: GET https://$DOMAIN_NAME/livez"

# On utilise -k pour accepter les certificats auto-signés ou en cours de propagation
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
