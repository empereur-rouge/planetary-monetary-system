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
# 2.5 Transfert des fichiers locaux (Force Sync)
# -----------------------------------------------------------------------------
# IMPORTANT: On envoie le docker-compose.yml local pour s'assurer que les
# modifications récentes (node1, healthcheck) sont bien prises en compte
# même sans git push.
echo ""
echo -e "${YELLOW}📤 Syncing local configuration files...${NC}"
scp docker-compose.yml Dockerfile "$VPS_USER@$VPS_IP:/opt/pms/"
echo -e "${GREEN}✅ Local docker-compose.yml & Dockerfile uploaded.${NC}"

# -----------------------------------------------------------------------------
# Execution sur le VPS
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}🔌 Connecting to VPS...${NC}"

# On construit un script HEREDOC dynamique pour envoyer les variables
ssh -T $VPS_USER@$VPS_IP << EOFREMOTE
set -e

# Définition des variables sur le distant
ADMIN_TOKEN="$ADMIN_TOKEN"
DOMAIN_NAME="$DOMAIN_NAME"
DO_GIT_PULL="$DO_GIT_PULL"
DO_BUILD="$DO_BUILD"
DO_CLEAN_RESET="$DO_CLEAN_RESET"
DO_INIT_COORD="$DO_INIT_COORD"

# Esthétique distante
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

echo "📁 Checking directories..."
mkdir -p /opt/pms
cd /opt/pms

# --- Action 1: Git Pull ---
if [ "\$DO_GIT_PULL" = "true" ]; then
    echo -e "\${YELLOW}📥 Updating repository...\${NC}"
    if [ -d ".git" ]; then
        # Force checkout config BEFORE pulling to avoid merge conflicts
        # But only if user accepts losing local changes to config
        # Here we assume yes for smooth deploy
        git checkout etc/config/config.prod.toml
        git pull
    else
        git clone https://github.com/empereur-rouge/planetary-monetary-system.git .
    fi
else
    echo "   Skipping Git Pull."
fi

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

# IMPORTANT: On utilise le docker-compose.yml du repo (déjà configuré pour la prod)
# Plus besoin de générer docker-compose.prod.yml dynamiquement

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
    email admin@localhost
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


# --- Action 2: Build & Start ---
if [ "$DO_BUILD" = "true" ]; then
    echo -e "${YELLOW}🐳 Rebuilding and Starting Containers...${NC}"
    
    # Nettoyage si demandé
    if [ "$DO_CLEAN_RESET" = "true" ]; then
        echo -e "${RED}🧹 Cleaning ALL data (down -v)...${NC}"
        docker compose -f docker-compose.yml down -v 2>/dev/null || true
    else
        docker compose -f docker-compose.yml down 2>/dev/null || true
    fi
    
    # Build
    # Note: On build pms-node qui contient tools-cli
    docker compose -f docker-compose.yml build --no-cache
    
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


# --- Action 3: Init Coordinator ---
if [ "\$DO_INIT_COORD" = "true" ]; then
    echo -e "\${YELLOW}👑 Initializing Coordinator...\${NC}"
    
    # On utilise tools-cli DANS le conteneur pour générer et mettre à jour la config
    # On redirige stderr vers null pour ne garder que le JSON clean sur stdout si possible
    # Mais tools-cli est verbeux. On va essayer de capturer le fichier généré.
    
    # Executer la génération
    docker exec pms-node-prod tools-cli gen-coordinator \
        /home/pms/config/pms/coordinator.key \
        /home/pms/config/pms/coordinator.json \
        /home/pms/config/config.prod.toml > /dev/null 2>&1
        
    # Redémarrer pour prendre en compte la nouvelle config (clés coordinator update)
    docker compose -f docker-compose.prod.yml restart node1
    
    echo "MAGIC_JSON_START"
    cat etc/pms/coordinator.json
    echo "MAGIC_JSON_END"
fi

EOFREMOTE

# -----------------------------------------------------------------------------
# Post-Processing Local (Affichage Secret)
# -----------------------------------------------------------------------------

echo ""
echo "================================================================================"
echo -e "🎉 ${GREEN}DEPLOYMENT FINISHED${NC}"
echo "================================================================================"
echo ""
echo -e "🌍 API Endpoint:     ${GREEN}https://$DOMAIN_NAME${NC}"
echo -e "🔑 Admin Token:      ${GREEN}$ADMIN_TOKEN${NC}"

echo ""
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
