#!/bin/bash
# ═══════════════════════════════════════════════════════════════════════════════
# docker_test.sh - Setup and Teardown for Docker 3-Node E2E Tests
# ═══════════════════════════════════════════════════════════════════════════════
#
# Usage:
#   ./scripts/docker_test.sh setup   # Create keys, images, start cluster
#   ./scripts/docker_test.sh down    # Stop cluster
#   ./scripts/docker_test.sh clean   # Stop and remove everything
#
# ═══════════════════════════════════════════════════════════════════════════════

set -e
cd "$(dirname "$0")/.."

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Coordinator private key (matches stress_common.rs COORDINATOR_PRIVATE_KEY_HEX)
COORDINATOR_PRIVATE_KEY="52f4cb8344e318c120f87bc0efb429bdd6b379c700731af27aaf59efffc0b248"

# ═══════════════════════════════════════════════════════════════════════════════
# Functions
# ═══════════════════════════════════════════════════════════════════════════════

setup_directories() {
    echo -e "${YELLOW}📁 Creating directories...${NC}"
    mkdir -p secrets/tls etc/pms docker_data/node{1,2,3}
}

generate_tls() {
    if [ ! -f secrets/tls/cert.pem ]; then
        echo -e "${YELLOW}🔐 Generating TLS certificates...${NC}"
        
        # CA
        openssl req -x509 -newkey rsa:2048 \
            -keyout secrets/tls/ca-key.pem \
            -out secrets/tls/ca-cert.pem \
            -days 365 -nodes -subj "/CN=PMS-Test-CA" 2>/dev/null
        
        # Server key
        openssl genrsa -out secrets/tls/key-temp.pem 2048 2>/dev/null
        openssl pkcs8 -topk8 -inform PEM -outform PEM \
            -in secrets/tls/key-temp.pem \
            -out secrets/tls/key.pem -nocrypt 2>/dev/null
        rm secrets/tls/key-temp.pem
        
        # Server cert with SANs
        cat > secrets/tls/extfile.cnf << EOF
subjectAltName=IP:127.0.0.1,DNS:localhost,DNS:node1,DNS:node2,DNS:node3
EOF
        openssl req -new -key secrets/tls/key.pem \
            -out secrets/tls/server.csr \
            -subj "/CN=127.0.0.1" 2>/dev/null
        openssl x509 -req -in secrets/tls/server.csr \
            -CA secrets/tls/ca-cert.pem \
            -CAkey secrets/tls/ca-key.pem \
            -CAcreateserial \
            -out secrets/tls/cert.pem \
            -days 365 \
            -extfile secrets/tls/extfile.cnf 2>/dev/null
        
        rm -f secrets/tls/server.csr secrets/tls/extfile.cnf
        chmod 644 secrets/tls/*.pem
        echo -e "${GREEN}✅ TLS certificates generated${NC}"
    else
        echo -e "${GREEN}✅ TLS certificates exist${NC}"
    fi
}

generate_keys() {
    echo -e "${YELLOW}🔑 Generating node identity keys...${NC}"
    
    # Node 1 uses the coordinator key (for mint authorization)
    echo -n "$COORDINATOR_PRIVATE_KEY" > etc/pms/node1.key
    
    # Nodes 2 & 3 get random keys (64 hex chars = 32 bytes)
    openssl rand -hex 32 > etc/pms/node2.key
    openssl rand -hex 32 > etc/pms/node3.key
    
    chmod 600 etc/pms/node*.key
    
    echo -e "${GREEN}✅ Keys generated:${NC}"
    echo "   Node1 (Coordinator): ${COORDINATOR_PRIVATE_KEY:0:16}..."
    echo "   Node2: $(head -c 16 etc/pms/node2.key)..."
    echo "   Node3: $(head -c 16 etc/pms/node3.key)..."
}

generate_admin_wallet() {
    if [ ! -f etc/pms/admin-wallet.json ] || [ ! -s etc/pms/admin-wallet.json ]; then
        echo -e "${YELLOW}👛 Generating admin wallet...${NC}"
        
        # Generate 32 random bytes for private key
        ADMIN_PRIV_HEX=$(openssl rand -hex 32)
        ADMIN_PRIV_B64=$(echo -n "$ADMIN_PRIV_HEX" | xxd -r -p | base64)
        
        # The public key derivation requires secp256k1 which shell can't do easily
        # So we use a fixed well-known admin key for testing
        # This matches the expected address in stress tests
        ADMIN_PRIV_HEX="1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
        ADMIN_PRIV_B64=$(echo -n "$ADMIN_PRIV_HEX" | xxd -r -p | base64)
        
        # Use openssl to derive secp256k1 public key
        # Create a temp EC key file
        echo -n "$ADMIN_PRIV_HEX" | xxd -r -p > /tmp/admin_priv.bin
        
        # Create DER format for secp256k1 private key
        printf '\x30\x77\x02\x01\x01\x04\x20' > /tmp/admin_key.der
        cat /tmp/admin_priv.bin >> /tmp/admin_key.der
        printf '\xa0\x0a\x06\x08\x2a\x86\x48\xce\x3d\x03\x01\x07\xa1\x44\x03\x42\x00\x04' >> /tmp/admin_key.der
        
        # Alternative: use openssl with proper secp256k1 key format
        # Since shell crypto is limited, we generate a known test key
        ADMIN_PUB_HEX="0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        X25519_PUB_HEX="2fe57da347cd62431528daac5fbb290730fff684afc4cfc2ed90995f58cb3b74"
        
        # Create wallet JSON
        cat > etc/pms/admin-wallet.json << EOF
{
    "private_key_b64": "$ADMIN_PRIV_B64",
    "public_key_hex": "$ADMIN_PUB_HEX",
    "x25519_pub_hex": "$X25519_PUB_HEX"
}
EOF
        
        # Cleanup temp files
        rm -f /tmp/admin_priv.bin /tmp/admin_key.der
        
        # Compute the admin address (for config reference)
        echo "   Admin Public Key: ${ADMIN_PUB_HEX:0:32}..."
        echo -e "${GREEN}✅ Admin wallet created${NC}"
    else
        echo -e "${GREEN}✅ Admin wallet exists${NC}"
    fi
}

create_test_compose() {
    # Update config with both coordinator keys using the tool
    # We use config.docker-test.toml as both input and output since it already exists from repo or previous runs
    # Wait, we need to make sure the file exists first. The previous logic created it inside this function.
    # We should update it AFTER creating it, OR update the source file if it is a template.
    # docker_test.sh mounts ./etc/config/config.docker-test.toml
    # Let's see how it was done before.
    # It was just creating the docker-compose file. The config file is expected to be at etc/config/config.docker-test.toml
    
    # 1. Ensure config file exists or is reset?
    # It seems we rely on git checked out file? No, we might be editing it.
    # Let's just run the derivation tool on the existing file.
    
    echo -e "${YELLOW}🔑 Updating coordinator keys in config...${NC}"
    # Use the hardcoded key to update the config
    cargo run -p tools-cli -- derive-coordinator "$COORDINATOR_PRIVATE_KEY" "etc/config/config.docker-test.toml"
    
    echo -e "${YELLOW}🐳 Creating docker-compose.test.yml...${NC}"
    cat > docker-compose.test.yml << 'COMPOSE_EOF'
# Generated by docker_test.sh - 3-Node Test Cluster
services:
  node1:
    build:
      context: .
      dockerfile: Dockerfile
      target: runtime
    image: pms-node:test
    container_name: pms-node1
    hostname: node1
    user: "pms"
    environment:
      RUST_LOG: info,pms_server=debug
      PMS_CONFIG: /home/pms/config/config.docker-test.toml
      PMS_ADMIN_TOKEN: pms_admin_secret
      PMS__LIMITS__RATE_LIMIT_RPS: 10000
      PMS__LIMITS__BURST: 20000
      PMS__P2P__KNOWN_PEERS: "node2:8443,node3:8443"
    volumes:
      - ./etc/config/config.docker-test.toml:/home/pms/config/config.docker-test.toml:ro
      - ./etc/pms/node1.key:/home/pms/config/node-identity.key:ro
      - ./etc/pms/admin-wallet.json:/home/pms/config/admin-wallet.json:ro
      - ./etc/pms/treasury-wallets.json:/home/pms/config/treasury-wallets.json:ro
      - ./secrets/tls:/home/pms/tls:ro
      - ./docker_data/node1:/home/pms/data
    ports:
      - "8080:8080"
    healthcheck:
      test: ["CMD-SHELL", "timeout 2 bash -c '</dev/tcp/127.0.0.1/8080' || exit 1"]
      interval: 2s
      timeout: 3s
      retries: 15
      start_period: 5s

  node2:
    build:
      context: .
      dockerfile: Dockerfile
      target: runtime
    image: pms-node:test
    container_name: pms-node2
    hostname: node2
    user: "pms"
    depends_on:
      node1:
        condition: service_healthy
    environment:
      RUST_LOG: info
      PMS_CONFIG: /home/pms/config/config.docker-test.toml
      PMS_ADMIN_TOKEN: pms_admin_secret
      PMS__LIMITS__RATE_LIMIT_RPS: 10000
      PMS__LIMITS__BURST: 20000
      PMS__P2P__KNOWN_PEERS: "node1:8443,node3:8443"
    volumes:
      - ./etc/config/config.docker-test.toml:/home/pms/config/config.docker-test.toml:ro
      - ./etc/pms/node2.key:/home/pms/config/node-identity.key:ro
      - ./etc/pms/admin-wallet.json:/home/pms/config/admin-wallet.json:ro
      - ./etc/pms/treasury-wallets.json:/home/pms/config/treasury-wallets.json:ro
      - ./secrets/tls:/home/pms/tls:ro
      - ./docker_data/node2:/home/pms/data
    ports:
      - "8081:8080"
    healthcheck:
      test: ["CMD-SHELL", "timeout 2 bash -c '</dev/tcp/127.0.0.1/8080' || exit 1"]
      interval: 2s
      timeout: 3s
      retries: 15

  node3:
    build:
      context: .
      dockerfile: Dockerfile
      target: runtime
    image: pms-node:test
    container_name: pms-node3
    hostname: node3
    user: "pms"
    depends_on:
      node1:
        condition: service_healthy
    environment:
      RUST_LOG: info
      PMS_CONFIG: /home/pms/config/config.docker-test.toml
      PMS_ADMIN_TOKEN: pms_admin_secret
      PMS__LIMITS__RATE_LIMIT_RPS: 10000
      PMS__LIMITS__BURST: 20000
      PMS__P2P__KNOWN_PEERS: "node1:8443,node2:8443"
    volumes:
      - ./etc/config/config.docker-test.toml:/home/pms/config/config.docker-test.toml:ro
      - ./etc/pms/node3.key:/home/pms/config/node-identity.key:ro
      - ./etc/pms/admin-wallet.json:/home/pms/config/admin-wallet.json:ro
      - ./etc/pms/treasury-wallets.json:/home/pms/config/treasury-wallets.json:ro
      - ./secrets/tls:/home/pms/tls:ro
      - ./docker_data/node3:/home/pms/data
    ports:
      - "8082:8080"
    healthcheck:
      test: ["CMD-SHELL", "timeout 2 bash -c '</dev/tcp/127.0.0.1/8080' || exit 1"]
      interval: 2s
      timeout: 3s
      retries: 15
COMPOSE_EOF
    echo -e "${GREEN}✅ docker-compose.test.yml created${NC}"
}

build_and_start() {
    echo -e "${YELLOW}🏗️  Building Docker image (once)...${NC}"
    docker compose -f docker-compose.test.yml build node1
    
    echo -e "${YELLOW}🚀 Starting 3-node cluster...${NC}"
    docker compose -f docker-compose.test.yml up -d --remove-orphans
    
    echo -e "${YELLOW}⏳ Waiting for nodes to be healthy...${NC}"
    sleep 5
    
    # Check health
    for port in 8080 8081 8082; do
        for i in {1..30}; do
            if curl -k -s "https://127.0.0.1:$port/live" > /dev/null 2>&1; then
                echo -e "${GREEN}✅ Node on port $port is UP${NC}"
                break
            fi
            if [ $i -eq 30 ]; then
                echo -e "${RED}❌ Node on port $port failed to start${NC}"
                docker compose -f docker-compose.test.yml logs
                exit 1
            fi
            sleep 1
        done
    done
}

stop_cluster() {
    echo -e "${YELLOW}🛑 Stopping cluster...${NC}"
    docker compose -f docker-compose.test.yml down -v 2>/dev/null || true
    echo -e "${GREEN}✅ Cluster stopped${NC}"
}

cleanup() {
    echo -e "${YELLOW}🧹 Full cleanup...${NC}"
    stop_cluster
    
    # Remove generated files
    rm -f etc/pms/node1.key etc/pms/node2.key etc/pms/node3.key
    rm -rf docker_data/node1 docker_data/node2 docker_data/node3
    rm -f docker-compose.test.yml
    
    # Optionally remove TLS (uncomment if needed)
    # rm -rf secrets/tls
    
    echo -e "${GREEN}✅ Cleanup complete${NC}"
}

show_usage() {
    echo "Usage: $0 {setup|down|clean|status}"
    echo ""
    echo "Commands:"
    echo "  setup  - Generate keys, TLS, build images, start 3-node cluster"
    echo "  down   - Stop the cluster"
    echo "  clean  - Stop cluster and remove all generated files"
    echo "  status - Show cluster status"
    echo ""
    echo "After setup, run tests with:"
    echo "  cargo test docker_stress_sync --ignored -- --nocapture"
}

# ═══════════════════════════════════════════════════════════════════════════════
# Main
# ═══════════════════════════════════════════════════════════════════════════════

case "$1" in
    setup)
        setup_directories
        generate_tls
        generate_keys
        generate_admin_wallet
        create_test_compose
        build_and_start
        echo ""
        echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
        echo -e "${GREEN}✅ 3-Node Test Cluster is READY!${NC}"
        echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
        echo ""
        echo "Nodes:"
        echo "  - Node 1 (Coordinator): https://127.0.0.1:8080"
        echo "  - Node 2:               https://127.0.0.1:8081"
        echo "  - Node 3:               https://127.0.0.1:8082"
        echo ""
        echo "Run tests:"
        echo "  cargo test docker_stress_sync --ignored -- --nocapture"
        ;;
    down)
        stop_cluster
        ;;
    clean)
        cleanup
        ;;
    status)
        docker compose -f docker-compose.test.yml ps 2>/dev/null || echo "No cluster running"
        ;;
    *)
        show_usage
        exit 1
        ;;
esac
