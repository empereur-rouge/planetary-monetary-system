#!/bin/bash
# =============================================================================
# Script de déploiement PMS sur VPS
# =============================================================================
# Usage: ./deploy.sh <VPS_IP> [USER]
#
# Ce script:
# 1. Build l'image Docker
# 2. Génère les clés de nœud
# 3. Copie les fichiers vers le VPS
# 4. Lance les conteneurs

set -e

VPS_IP="${1:-}"
VPS_USER="${2:-root}"

if [ -z "$VPS_IP" ]; then
    echo "Usage: $0 <VPS_IP> [USER]"
    echo "Example: $0 123.45.67.89 ubuntu"
    exit 1
fi

echo "🚀 Déploiement PMS sur $VPS_USER@$VPS_IP"

# =============================================================================
# 1. Build local
# =============================================================================
echo "📦 Build de l'image Docker..."
docker build -t pms-node:latest .

echo "💾 Export de l'image..."
docker save pms-node:latest | gzip > /tmp/pms-node.tar.gz

# =============================================================================
# 2. Génération des clés (si absentes)
# =============================================================================
if [ ! -f etc/pms/node1.key ]; then
    echo "🔑 Génération des clés de nœuds..."
    mkdir -p etc/pms
    for i in 1 2 3; do
        openssl ecparam -name secp256k1 -genkey -noout -out etc/pms/node$i.key 2>/dev/null
    done
fi

if [ ! -f etc/pms/admin-wallet.json ]; then
    echo "👛 Génération du wallet admin..."
    # Crée un wallet admin basique (à remplacer en prod)
    echo '{"note": "Replace with real wallet"}' > etc/pms/admin-wallet.json
fi

# =============================================================================
# 3. Génération certificats TLS auto-signés (dev only)
# =============================================================================
if [ ! -f secrets/tls/cert.pem ]; then
    echo "🔐 Génération certificats TLS (dev)..."
    mkdir -p secrets/tls
    openssl req -x509 -newkey rsa:4096 -keyout secrets/tls/key.pem \
        -out secrets/tls/cert.pem -days 365 -nodes \
        -subj "/CN=pms-node" 2>/dev/null
fi

# =============================================================================
# 4. Copie vers VPS
# =============================================================================
echo "📤 Copie des fichiers vers le VPS..."
ssh $VPS_USER@$VPS_IP "mkdir -p /opt/pms/{etc,secrets,docker_data}"

scp /tmp/pms-node.tar.gz $VPS_USER@$VPS_IP:/opt/pms/
scp docker-compose.yml $VPS_USER@$VPS_IP:/opt/pms/
scp caddyfile $VPS_USER@$VPS_IP:/opt/pms/
scp -r etc/pms $VPS_USER@$VPS_IP:/opt/pms/etc/
scp -r etc/config $VPS_USER@$VPS_IP:/opt/pms/etc/
scp -r secrets/tls $VPS_USER@$VPS_IP:/opt/pms/secrets/

# =============================================================================
# 5. Lancement sur le VPS
# =============================================================================
echo "🐳 Lancement des conteneurs..."
ssh $VPS_USER@$VPS_IP << 'EOF'
cd /opt/pms
docker load < pms-node.tar.gz
rm pms-node.tar.gz

# Créer les dossiers de données
mkdir -p docker_data/node{1,2,3}
chmod -R 777 docker_data

# Lancer
docker compose up -d
docker compose ps
EOF

echo ""
echo "✅ Déploiement terminé!"
echo ""
echo "📊 Vérifier le statut:"
echo "   ssh $VPS_USER@$VPS_IP 'docker compose -f /opt/pms/docker-compose.yml ps'"
echo ""
echo "📜 Voir les logs:"
echo "   ssh $VPS_USER@$VPS_IP 'docker compose -f /opt/pms/docker-compose.yml logs -f'"
echo ""
echo "🌐 API disponible sur:"
echo "   - http://$VPS_IP:8080 (node1)"
echo "   - http://$VPS_IP:8081 (node2)"
echo "   - http://$VPS_IP:8082 (node3)"
