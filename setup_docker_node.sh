#!/bin/bash
set -e
# --- Configuration ---
DATA_DIR="./docker_data"
CONFIG_SRC="./etc/config/config.prod.toml"
COMPOSE_FILE="./docker-compose.yml"
echo "🐳 Setup PMS Node (Docker Edition)..."
# 1. Vérif Docker
if ! command -v docker &> /dev/null; then
    echo "❌ Docker n'est pas installé !"
    exit 1
fi
# 2. Préparation des Dossiers Hôte
echo "--- 🧱 Préparation des dossiers ---"
mkdir -p secrets/tls
mkdir -p etc/pms
mkdir -p docker_data
# 3. Génération de clés bidons si absentes (pour test uniquement)
if [ ! -f secrets/tls/cert.pem ]; then
    echo "⚠️  Certificats TLS absents, création de certificats auto-signés (RSA 2048 + SAN v3)..."
    openssl req -x509 -newkey rsa:2048 -nodes -keyout secrets/tls/key.pem -out secrets/tls/cert.pem -days 365 -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost"
    chmod 644 secrets/tls/key.pem
fi
if [ ! -f etc/pms/node-identity.key ]; then
    echo "⚠️  Clé Node Identity absente, création..."
    # On laisse le noeud la générer ? Non, il faut qu'elle soit montée.
    # Astuce: on génère 32 bytes random
    head -c 32 /dev/urandom > etc/pms/node-identity.key
fi
if [ ! -f etc/pms/admin-wallet.json ]; then
    echo "ℹ️  Admin Wallet absent, un fichier vide est créé (le noeud peut râler s'il veut charger le wallet)."
    echo "{}" > etc/pms/admin-wallet.json
fi
# 4. Permissions (User 1000:1000 par défaut ou celui du Dockerfile ?)
# Dans le Dockerfile, on a créé un user 'pms'. Son UID est incertain (souvent 1000 ou 1001).
# On change les perms pour que tout le monde puisse lire (en prod, affiner l'UID).
chmod -R 755 secrets/tls docker_data etc/pms
chmod 644 secrets/tls/key.pem
echo "--- 🏗️  Build & Start ---"
docker compose up -d --build --remove-orphans
echo ""
echo "✅ PMS Node démarré en Docker !"
echo "👉 Logs : docker compose logs -f node"
echo "👉 CLI  : docker compose run --rm cli"