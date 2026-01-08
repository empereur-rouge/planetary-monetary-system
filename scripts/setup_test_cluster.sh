#!/bin/bash
set -e
# setup_test_cluster.sh - For LOCAL TESTING with 3 nodes
# Use setup_docker_node.sh for PRODUCTION deployments.

echo "🧪 Setting up PMS Test Cluster (3 Nodes)..."

# 1. Directories
mkdir -p secrets/tls etc/pms docker_data

# 2. TLS Certs (local self-signed)
if [ ! -f secrets/tls/cert.pem ]; then
    echo "🔐 Generating local TLS PKI..."
    openssl req -x509 -newkey rsa:2048 -keyout secrets/tls/ca-key.pem -out secrets/tls/ca-cert.pem -days 365 -nodes -subj "/CN=PMS-Test-CA"
    openssl genrsa -out secrets/tls/key.pem 2048
    openssl pkcs8 -topk8 -inform PEM -outform PEM -in secrets/tls/key.pem -out secrets/tls/key_pk8.pem -nocrypt
    mv secrets/tls/key_pk8.pem secrets/tls/key.pem
    openssl req -new -key secrets/tls/key.pem -out secrets/tls/server.csr -subj "/CN=127.0.0.1"
    echo "subjectAltName=IP:127.0.0.1,DNS:localhost,DNS:node1,DNS:node2,DNS:node3" > secrets/tls/extfile.cnf
    openssl x509 -req -in secrets/tls/server.csr -CA secrets/tls/ca-cert.pem -CAkey secrets/tls/ca-key.pem -CAcreateserial -out secrets/tls/cert.pem -days 365 -extfile secrets/tls/extfile.cnf
    rm secrets/tls/server.csr secrets/tls/extfile.cnf
    chmod 644 secrets/tls/*.pem
    echo "✅ TLS Certs generated."
fi

# 3. Node Identity Keys (3 distinct keys for the cluster)
for i in 1 2 3; do
    if [ ! -f etc/pms/node$i.key ]; then
        echo "🔑 Generating key for Node $i..."
        head -c 32 /dev/urandom > etc/pms/node$i.key
    fi
done

# 4. Admin Wallet placeholder
if [ ! -f etc/pms/admin-wallet.json ]; then
    echo "{}" > etc/pms/admin-wallet.json
fi

# 5. Permissions
chmod -R 755 secrets/tls etc/pms
chmod -R 777 docker_data
chmod 644 secrets/tls/key.pem

# 6. Build & Start Cluster
echo "🏗️  Building and starting 3-node cluster..."
docker compose up -d --build --remove-orphans

echo ""
echo "✅ Test Cluster is UP!"
echo "👉 Logs: docker compose logs -f"
echo "👉 Run Test: cargo test --test docker_stress_sync -- --nocapture"
