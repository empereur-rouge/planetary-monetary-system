#!/bin/bash
# =============================================================================
# Script de déploiement PMS Interactif
# =============================================================================
# Usage: ./deploy.sh <VPS_IP> [USER]
#
# Ce script guide l'utilisateur à travers les étapes de déploiement:
# 1. Configuration (Admin Token)
# 2. Mise à jour (Git Pull)
# 3. Build & Restart (Docker)
# 4. Initialisation Coordinateur (Optionnel)

set -e

VPS_IP="${1:-}"
VPS_USER="${2:-root}"

# Couleurs
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

if [ -z "$VPS_IP" ]; then
    echo "Usage: $0 <VPS_IP> [USER]"
    echo "Example: $0 87.106.50.82 root"
    exit 1
fi

echo -e "${YELLOW}🚀 PMS Interactive Deployment Tool${NC}"
echo "   Target: $VPS_USER@$VPS_IP"
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

DO_INIT_COORD=false
if ask_yes_no "   ❓ Initialize/Reset Coordinator Wallet (DANGEROUS if already set)?" "N"; then
    DO_INIT_COORD=true
fi

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
DO_GIT_PULL="$DO_GIT_PULL"
DO_BUILD="$DO_BUILD"
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
# On met à jour le token dans le fichier .env pour docker compose
echo "PMS_ADMIN_TOKEN=\$ADMIN_TOKEN" > .env

# On assure que la config prod existe (sans écraser si existante, sauf si git pull l'a fait)
if [ ! -f etc/config/config.prod.toml ]; then
    cp etc/config/config.prod.template.toml etc/config/config.prod.toml
    # Ajustement des chemins pour Docker (home/pms/data vs /data/pms)
    sed -i 's|/data/pms/rocks|/home/pms/data/rocks|g' etc/config/config.prod.toml
    sed -i 's|/etc/pms/tls|/home/pms/tls|g' etc/config/config.prod.toml
    sed -i 's|ca_pem =|# ca_pem =|g' etc/config/config.prod.toml
fi


# --- Action 2: Build & Start ---
if [ "\$DO_BUILD" = "true" ]; then
    echo -e "\${YELLOW}🐳 Rebuilding and Starting Containers...\${NC}"
    
    # Nettoyage
    docker compose down 2>/dev/null || true
    
    # Build
    # Note: On build pms-node qui contient tools-cli
    docker compose build --no-cache
    
    # Start
    docker compose up -d

    echo -e "\${GREEN}✅ Containers started.\${NC}"
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
    docker exec pms-node tools-cli gen-coordinator \
        /home/pms/config/pms/coordinator.key \
        /home/pms/config/pms/coordinator.json \
        /home/pms/config/config.prod.toml > /dev/null 2>&1
        
    # Redémarrer pour prendre en compte la nouvelle config (clés coordinator update)
    docker compose restart node
    
    echo "MAGIC_JSON_START"
    cat etc/pms/coordinator.json
    echo "MAGIC_JSON_END"
fi

EOFREMOTE

# -----------------------------------------------------------------------------
# Post-Processing Local (Affichage Secret)
# -----------------------------------------------------------------------------

# Pour récupérer le JSON, c'est un peu tricky avec SSH interactif
# On va afficher un gros bloc résumé à la fin avec les infos qu'on a localement

echo ""
echo "================================================================================"
echo -e "🎉 ${GREEN}DEPLOYMENT FINISHED${NC}"
echo "================================================================================"
echo ""
echo -e "🌍 API Endpoint:     ${GREEN}https://$VPS_IP:8080${NC}"
echo -e "🔑 Admin Token:      ${GREEN}$ADMIN_TOKEN${NC}"
echo ""

if [ "$DO_INIT_COORD" = "true" ]; then
    echo -e "${RED}⚠️  COORDINATOR INITIALIZED${NC}"
    echo "   Check the output above for a JSON block containing your PRIVATE KEY and MNEMONIC."
    echo "   You must verify the key was generated on the server."
    echo ""
    echo "   To retrieve it manually if you missed it:"
    echo "   ssh $VPS_USER@$VPS_IP 'cat /opt/pms/etc/pms/coordinator.json'"
fi

echo ""
