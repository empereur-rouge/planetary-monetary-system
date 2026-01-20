#!/bin/bash
# =============================================================================
# Script de déploiement PMS sur VPS (Production - Single Node)
# =============================================================================
# Usage: ./deploy.sh <VPS_IP> [USER] [ADMIN_TOKEN]
#
# Ce script:
# 1. Push les derniers changements sur GitHub
# 2. Se connecte au VPS
# 3. Pull le code, génère les clés/certs si nécessaire
# 4. Build l'image Docker directement sur le VPS
# 5. Crée config.prod.toml avec le token admin
# 6. Lance le conteneur

set -e

VPS_IP="${1:-}"
VPS_USER="${2:-root}"
ADMIN_TOKEN="${3:-}"

if [ -z "$VPS_IP" ]; then
    echo "Usage: $0 <VPS_IP> [USER] [ADMIN_TOKEN]"
    echo "Example: $0 87.106.50.82 root mon_token_secret"
    exit 1
fi

# Demander le token si non fourni
if [ -z "$ADMIN_TOKEN" ]; then
    echo "🔐 Entrez votre token admin (ou appuyez sur Entrée pour en générer un nouveau):"
    read -s ADMIN_TOKEN
    if [ -z "$ADMIN_TOKEN" ]; then
        ADMIN_TOKEN=$(openssl rand -hex 32)
        echo "📝 Token généré: $ADMIN_TOKEN"
        echo "⚠️  Sauvegardez ce token, il ne sera plus affiché!"
    fi
fi

echo ""
echo "🚀 Déploiement PMS sur $VPS_USER@$VPS_IP"
echo "=================================================="

# =============================================================================
# 1. Push local vers GitHub
# =============================================================================
echo ""
echo "📤 Push des derniers changements vers GitHub..."
git add . 2>/dev/null || true
git commit -m "Deploy $(date +%Y-%m-%d_%H:%M)" 2>/dev/null || echo "   (Rien à committer)"
git push 2>/dev/null || echo "   (Rien à pusher)"

# =============================================================================
# 2. Déploiement sur le VPS
# =============================================================================
echo ""
echo "🐳 Déploiement sur le VPS..."
ssh $VPS_USER@$VPS_IP << EOFREMOTE
set -e

echo "📁 Préparation des dossiers..."
mkdir -p /opt/pms
cd /opt/pms

# Clone ou pull
if [ -d ".git" ]; then
    echo "📥 Mise à jour du code..."
    git pull
else
    echo "📥 Clonage du repo..."
    git clone https://github.com/empereur-rouge/planetary-monetary-system.git .
fi

echo "🔑 Génération des clés (si nécessaire)..."
mkdir -p etc/pms secrets/tls

# Clé du nœud (format hex - 64 caractères)
if [ ! -f etc/pms/node.key ] || [ \$(stat -c%s etc/pms/node.key 2>/dev/null || echo 0) -gt 100 ]; then
    # Générer 32 bytes aléatoires et les convertir en hex
    openssl rand -hex 32 > etc/pms/node.key
    echo "   ✓ node.key créée (format hex)"
fi

# Fixer les permissions
chmod 644 etc/pms/node.key

# Certificats TLS
if [ ! -f secrets/tls/cert.pem ]; then
    openssl req -x509 -newkey rsa:4096 -keyout secrets/tls/key.pem \
        -out secrets/tls/cert.pem -days 365 -nodes \
        -subj "/CN=pms-node" 2>/dev/null
    echo "   ✓ Certificats TLS créés"
fi

# Fixer permissions TLS
chmod 644 secrets/tls/*.pem

# Admin wallet vide si absent
if [ ! -f etc/pms/admin-wallet.json ]; then
    echo '{}' > etc/pms/admin-wallet.json
    echo "   ✓ admin-wallet.json créé"
fi
chmod 644 etc/pms/admin-wallet.json

echo "📝 Configuration prod..."
if [ -f etc/config/config.prod.toml ]; then
    echo "   ✓ config.prod.toml existe déjà, conservation des modifications"
else
    echo "   → Création depuis le template..."
    cp etc/config/config.prod.template.toml etc/config/config.prod.toml
    sed -i 's/REPLACE_WITH_YOUR_SECRET_TOKEN/${ADMIN_TOKEN}/' etc/config/config.prod.toml
    echo "   ✓ config.prod.toml créée"
fi
chmod 644 etc/config/config.prod.toml

echo "🛑 Arrêt des anciens conteneurs..."
docker compose down 2>/dev/null || true

echo "🗑️  Nettoyage de l'ancienne image..."
docker rmi pms-node:latest 2>/dev/null || true

echo "🔨 Build de l'image Docker (peut prendre plusieurs minutes)..."
docker build -t pms-node:latest .

echo "🚀 Lancement du conteneur..."
docker compose up -d

echo ""
echo "📊 Statut:"
docker compose ps

echo ""
echo "📜 Derniers logs:"
sleep 3
docker compose logs --tail 10

EOFREMOTE

echo ""
echo "=================================================="
echo "✅ Déploiement terminé!"
echo ""
echo "📊 Commandes utiles:"
echo "   ssh $VPS_USER@$VPS_IP 'cd /opt/pms && docker compose ps'"
echo "   ssh $VPS_USER@$VPS_IP 'cd /opt/pms && docker compose logs -f'"
echo ""
echo "🌐 API disponible sur:"
echo "   https://$VPS_IP:8080/livez"
echo ""
echo "🔐 Token admin: $ADMIN_TOKEN"
echo "   (Gardez-le en sécurité!)"
