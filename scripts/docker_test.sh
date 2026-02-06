#!/bin/bash
# ═══════════════════════════════════════════════════════════════════════════════
# docker_test.sh - VPS Production Architecture (Single Writer Mode)
# ═══════════════════════════════════════════════════════════════════════════════
#
# Architecture (4 VPS Production Setup):
#   ┌──────────────────┐     ┌──────────────────┐
#   │  VPS 1: Gateway  │────▶│  VPS 2: Engine   │
#   │  (Public Entry)  │     │  (Coordinator)   │
#   │  Port: 8443      │     │  Internal Only   │
#   └──────────────────┘     └────────┬─────────┘
#                                     │
#                                     ▼
#   ┌──────────────────┐     ┌──────────────────┐
#   │  VPS 4: Metrics  │     │  VPS 3: RocksDB  │
#   │  (Prometheus)    │◀────│  (Data Volume)   │
#   │  Port: 9091      │     │  Persistent      │
#   └──────────────────┘     └──────────────────┘
#
# Gateway is the ONLY public entry point - Engine is internal only!
#
# Usage:
#   ./scripts/docker_test.sh setup   # Build images, start full stack
#   ./scripts/docker_test.sh down    # Stop stack
#   ./scripts/docker_test.sh clean   # Stop and remove everything
#   ./scripts/docker_test.sh status  # Show stack status
#
# ═══════════════════════════════════════════════════════════════════════════════

set -e
cd "$(dirname "$0")/.."

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Generated wallets will be stored here
COORDINATOR_WALLET_JSON=""
TREASURY_WALLET_JSON=""

# ═══════════════════════════════════════════════════════════════════════════════
# Functions
# ═══════════════════════════════════════════════════════════════════════════════

setup_directories() {
    echo -e "${YELLOW}📁 Creating directories...${NC}"
    mkdir -p secrets/tls etc/pms docker_data/node etc/prometheus
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
        
        # Server cert with SANs for internal Docker networking
        cat > secrets/tls/extfile.cnf << EOF
subjectAltName=IP:127.0.0.1,DNS:localhost,DNS:pms-engine,DNS:pms-gateway,DNS:engine,DNS:gateway
EOF
        openssl req -new -key secrets/tls/key.pem \
            -out secrets/tls/server.csr \
            -subj "/CN=pms-engine" 2>/dev/null
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
    echo -e "${YELLOW}🔑 Generating coordinator wallet with mnemonic...${NC}"

    # Generate a real wallet with mnemonic
    COORDINATOR_WALLET_JSON=$(cargo run -q -p tools-cli -- wallet-generate pms)

    # Extract private key hex for node.key
    COORDINATOR_PRIVATE_KEY=$(echo "$COORDINATOR_WALLET_JSON" | jq -r '.private_key_hex')
    echo -n "$COORDINATOR_PRIVATE_KEY" > etc/pms/node.key
    chmod 600 etc/pms/node.key

    # Save full wallet JSON for reference
    echo "$COORDINATOR_WALLET_JSON" > etc/pms/coordinator-wallet.json
    chmod 600 etc/pms/coordinator-wallet.json

    COORD_ADDR=$(echo "$COORDINATOR_WALLET_JSON" | jq -r '.address')
    echo -e "${GREEN}✅ Coordinator wallet generated:${NC}"
    echo "   Address: $COORD_ADDR"
}

generate_admin_wallet() {
    echo -e "${YELLOW}👛 Generating treasury wallet with mnemonic...${NC}"

    # Generate a real wallet with mnemonic for treasury
    TREASURY_WALLET_JSON=$(cargo run -q -p tools-cli -- wallet-generate pms)

    # Extract values
    TREASURY_PRIV_B64=$(echo "$TREASURY_WALLET_JSON" | jq -r '.private_key_b64')
    TREASURY_PUB_HEX=$(echo "$TREASURY_WALLET_JSON" | jq -r '.public_key_hex')
    TREASURY_X25519=$(echo "$TREASURY_WALLET_JSON" | jq -r '.x25519_pub_hex')
    TREASURY_ADDR=$(echo "$TREASURY_WALLET_JSON" | jq -r '.address')

    # Save as admin-wallet.json (used by engine)
    cat > etc/pms/admin-wallet.json << EOF
{
    "private_key_b64": "$TREASURY_PRIV_B64",
    "public_key_hex": "$TREASURY_PUB_HEX",
    "x25519_pub_hex": "$TREASURY_X25519"
}
EOF
    chmod 600 etc/pms/admin-wallet.json

    # Save full wallet JSON with mnemonic for reference
    echo "$TREASURY_WALLET_JSON" > etc/pms/treasury-wallet.json
    chmod 600 etc/pms/treasury-wallet.json

    echo -e "${GREEN}✅ Treasury wallet generated:${NC}"
    echo "   Address: $TREASURY_ADDR"
}

create_prometheus_config() {
    echo -e "${YELLOW}📊 Creating Prometheus config...${NC}"
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
    echo -e "${GREEN}✅ Prometheus config created${NC}"
}

create_test_compose() {
    echo -e "${YELLOW}🔑 Updating coordinator keys in config...${NC}"
    COORD_PRIV_KEY=$(cat etc/pms/coordinator-wallet.json | jq -r '.private_key_hex')
    cargo run -p tools-cli -- derive-coordinator "$COORD_PRIV_KEY" "etc/config/config.docker-test.toml"
    
    echo -e "${YELLOW}🐳 Creating docker-compose.test.yml (4-Service VPS Architecture)...${NC}"
    cat > docker-compose.test.yml << 'COMPOSE_EOF'
# ═══════════════════════════════════════════════════════════════════════════════
# VPS Production Architecture - Single Writer Mode
# ═══════════════════════════════════════════════════════════════════════════════
#
# All 4 services are MANDATORY:
# - pms-engine:   Coordinator Node (INTERNAL ONLY - no public ports!)
# - pms-gateway:  API Gateway (PUBLIC - sole entry point)
# - rocksdb:      Persistent storage volume (simulated via Docker volume)
# - prometheus:   Metrics collection
#
# ═══════════════════════════════════════════════════════════════════════════════

services:
  # ─────────────────────────────────────────────────────────────────────────────
  # VPS 2: PMS Engine (Coordinator) - INTERNAL ONLY
  # ─────────────────────────────────────────────────────────────────────────────
  # This is the Single Writer that validates and persists all blocks.
  # It is NOT exposed to the public - only Gateway can reach it.
  pms-engine:
    build:
      context: .
      dockerfile: Dockerfile
      target: runtime
    image: pms-node:test
    container_name: pms-engine
    hostname: pms-engine
    user: "pms"
    environment:
      RUST_LOG: info,pms_server=debug,pms_core=debug
      PMS_CONFIG: /home/pms/config/config.docker-test.toml
      PMS_ADMIN_TOKEN: pms_admin_secret
      PMS__LIMITS__RATE_LIMIT_RPS: 10000
      PMS__LIMITS__BURST: 20000
    volumes:
      - ./etc/config/config.docker-test.toml:/home/pms/config/config.docker-test.toml:ro
      - ./etc/pms/node.key:/home/pms/config/node-identity.key:ro
      - ./etc/pms/admin-wallet.json:/home/pms/config/admin-wallet.json:ro
      - ./etc/pms/treasury-wallets.json:/home/pms/config/treasury-wallets.json:ro
      - ./secrets/tls:/home/pms/tls:ro
      - rocksdb_data:/home/pms/data  # VPS 3: Persistent RocksDB storage
    # NO PUBLIC PORTS - Internal network only!
    # ports:
    #   - "8080:8080"  # DISABLED - Only Gateway can access Engine
    expose:
      - "8080"  # Internal Docker network only
    healthcheck:
      test: ["CMD-SHELL", "timeout 2 bash -c '</dev/tcp/127.0.0.1/8080' || exit 1"]
      interval: 2s
      timeout: 3s
      retries: 15
      start_period: 5s
    networks:
      - pms-internal

  # ─────────────────────────────────────────────────────────────────────────────
  # VPS 1: PMS Gateway - PUBLIC ENTRY POINT (sole access to network)
  # ─────────────────────────────────────────────────────────────────────────────
  # All external requests MUST go through Gateway.
  # Gateway proxies to Engine on internal network.
  pms-gateway:
    build:
      context: .
      dockerfile: Dockerfile.gateway
    image: pms-gateway:test
    container_name: pms-gateway
    hostname: pms-gateway
    depends_on:
      pms-engine:
        condition: service_healthy
    environment:
      RUST_LOG: info,pms_gateway=debug
      UPSTREAM_URL: https://pms-engine:8080
      LISTEN_ADDR: 0.0.0.0:8443
      TLS_CERT: /app/tls/cert.pem
      TLS_KEY: /app/tls/key.pem
      ADMIN_TOKEN: pms_admin_secret
      DASHBOARD_PATH: /app/dashboard
    volumes:
      - ./secrets/tls:/app/tls:ro
    ports:
      - "8443:8443"  # PUBLIC - The ONLY exposed port for external access
    healthcheck:
      test: ["CMD-SHELL", "timeout 2 bash -c '</dev/tcp/127.0.0.1/8443' || exit 1"]
      interval: 2s
      timeout: 3s
      retries: 15
    networks:
      - pms-internal
      - pms-public

  # ─────────────────────────────────────────────────────────────────────────────
  # VPS 4: Prometheus (Metrics) - Mandatory monitoring
  # ─────────────────────────────────────────────────────────────────────────────
  prometheus:
    image: prom/prometheus:v2.45.0
    container_name: pms-prometheus
    volumes:
      - ./etc/prometheus/prometheus.yml:/etc/prometheus/prometheus.yml:ro
      - prometheus_data:/prometheus
    ports:
      - "9091:9090"  # Prometheus Web UI
    command:
      - '--config.file=/etc/prometheus/prometheus.yml'
      - '--storage.tsdb.path=/prometheus'
      - '--web.enable-lifecycle'
    networks:
      - pms-internal

# ─────────────────────────────────────────────────────────────────────────────
# Networks
# ─────────────────────────────────────────────────────────────────────────────
networks:
  pms-internal:
    driver: bridge
    internal: true  # No external access - Engine is isolated
  pms-public:
    driver: bridge
    # Gateway connects to both internal (to reach Engine) and public (to serve clients)

# ─────────────────────────────────────────────────────────────────────────────
# Volumes (VPS 3: RocksDB Persistent Storage)
# ─────────────────────────────────────────────────────────────────────────────
volumes:
  rocksdb_data:
    driver: local
  prometheus_data:
    driver: local

COMPOSE_EOF
    echo -e "${GREEN}✅ docker-compose.test.yml created (4-Service VPS Architecture)${NC}"
}

build_and_start() {
    echo -e "${YELLOW}🏗️  Building Docker images...${NC}"
    
    # Build Engine image
    docker compose -f docker-compose.test.yml build pms-engine
    
    # Build Gateway image (uses existing Dockerfile.gateway with dashboard)
    echo -e "${YELLOW}🔧 Building Gateway image (with dashboard)...${NC}"
    docker compose -f docker-compose.test.yml build pms-gateway
    
    echo -e "${YELLOW}🚀 Starting 4-Service Stack...${NC}"
    docker compose -f docker-compose.test.yml up -d --remove-orphans
    
    echo -e "${YELLOW}⏳ Waiting for services to be healthy...${NC}"
    sleep 3
    
    # Check Engine health (internal)
    for i in {1..30}; do
        if docker exec pms-engine bash -c 'timeout 2 bash -c "</dev/tcp/127.0.0.1/8080"' 2>/dev/null; then
            echo -e "${GREEN}✅ Engine is UP (internal)${NC}"
            break
        fi
        if [ $i -eq 30 ]; then
            echo -e "${RED}❌ Engine failed to start${NC}"
            docker compose -f docker-compose.test.yml logs pms-engine
            exit 1
        fi
        sleep 1
    done
    
    # Check Gateway health (public)
    for i in {1..30}; do
        if curl -k -s "https://127.0.0.1:8443/livez" > /dev/null 2>&1; then
            echo -e "${GREEN}✅ Gateway is UP (public port 8443)${NC}"
            break
        fi
        if [ $i -eq 30 ]; then
            echo -e "${RED}❌ Gateway failed to start${NC}"
            docker compose -f docker-compose.test.yml logs pms-gateway
            exit 1
        fi
        sleep 1
    done
    
    # Check Prometheus
    for i in {1..15}; do
        if curl -s "http://127.0.0.1:9091/-/ready" > /dev/null 2>&1; then
            echo -e "${GREEN}✅ Prometheus is UP (port 9091)${NC}"
            break
        fi
        if [ $i -eq 15 ]; then
            echo -e "${YELLOW}⚠️ Prometheus may still be starting...${NC}"
        fi
        sleep 1
    done
}

generate_credentials_file() {
    echo -e "${YELLOW}📄 Generating credentials file...${NC}"

    TIMESTAMP=$(date "+%Y-%m-%d %H:%M:%S")
    OUTPUT_FILE="pms-credentials.txt"

    # Get coordinator info from generated wallet
    COORD_PRIV=$(cat etc/pms/coordinator-wallet.json | jq -r '.private_key_hex')
    COORD_PUB=$(cat etc/pms/coordinator-wallet.json | jq -r '.public_key_hex')
    COORD_WALLET=$(cat etc/pms/coordinator-wallet.json | jq -r '.address')
    COORD_MNEMONIC=$(cat etc/pms/coordinator-wallet.json | jq -r '.mnemonic')

    # Get treasury info from generated wallet
    TREASURY_PRIV=$(cat etc/pms/treasury-wallet.json | jq -r '.private_key_hex')
    TREASURY_WALLET=$(cat etc/pms/treasury-wallet.json | jq -r '.address')
    TREASURY_MNEMONIC=$(cat etc/pms/treasury-wallet.json | jq -r '.mnemonic')

    cat > "$OUTPUT_FILE" << EOF
═══════════════════════════════════════════════════════════════════════════════
                    PMS Docker Test - Credentials & Configuration
═══════════════════════════════════════════════════════════════════════════════
Generated: $TIMESTAMP

───────────────────────────────────────────────────────────────────────────────
                              SERVICES ENDPOINTS
───────────────────────────────────────────────────────────────────────────────
Gateway (Public):     https://127.0.0.1:8443
Dashboard:            https://127.0.0.1:8443/dashboard/
Prometheus:           http://127.0.0.1:9091
Engine (Internal):    https://pms-engine:8080 (Docker network only)

───────────────────────────────────────────────────────────────────────────────
                              COORDINATOR (Single Writer)
───────────────────────────────────────────────────────────────────────────────
Private Key (hex):    $COORD_PRIV
Public Key:           $COORD_PUB
Wallet Address:       $COORD_WALLET

⚠️  MNEMONIC (24 words) - SAVE THIS SECURELY:
$COORD_MNEMONIC

───────────────────────────────────────────────────────────────────────────────
                              TREASURY WALLET
───────────────────────────────────────────────────────────────────────────────
Private Key (hex):    $TREASURY_PRIV
Wallet Address:       $TREASURY_WALLET

⚠️  MNEMONIC (24 words) - SAVE THIS SECURELY:
$TREASURY_MNEMONIC

───────────────────────────────────────────────────────────────────────────────
                              ADMIN ACCESS
───────────────────────────────────────────────────────────────────────────────
Admin Token:          pms_admin_secret
Header:               Authorization: Bearer pms_admin_secret

───────────────────────────────────────────────────────────────────────────────
                              API EXAMPLES
───────────────────────────────────────────────────────────────────────────────
# Health check
curl -k https://127.0.0.1:8443/livez

# Get tips
curl -k https://127.0.0.1:8443/v1/tips

# Get supply
curl -k https://127.0.0.1:8443/v1/supply

# Get coordinator info
curl -k https://127.0.0.1:8443/v1/coordinator/info

# Admin: distribute fees
curl -k -X POST -H "Authorization: Bearer pms_admin_secret" \\
     https://127.0.0.1:8443/admin/distribute_fees

# Get UTXOs for treasury
curl -k https://127.0.0.1:8443/v1/utxos/$TREASURY_WALLET

───────────────────────────────────────────────────────────────────────────────
                              IMPORTANT NOTES
───────────────────────────────────────────────────────────────────────────────
- Gateway is the ONLY public entry point - Engine is internal only
- TLS certificates are self-signed (use -k with curl)
- This is a TEST environment - do not use these keys in production!
- SAVE THE MNEMONICS - they are the only way to recover the wallets!

═══════════════════════════════════════════════════════════════════════════════
EOF

    echo -e "${GREEN}✅ Credentials saved to: $OUTPUT_FILE${NC}"
}

stop_cluster() {
    echo -e "${YELLOW}🛑 Stopping stack...${NC}"
    docker compose -f docker-compose.test.yml down 2>/dev/null || true
    echo -e "${GREEN}✅ Stack stopped${NC}"
}

cleanup() {
    echo -e "${YELLOW}🧹 Full cleanup...${NC}"
    stop_cluster
    
    # Remove volumes
    docker compose -f docker-compose.test.yml down -v 2>/dev/null || true
    
    # Remove generated files
    rm -f etc/pms/node.key
    rm -f etc/pms/coordinator-wallet.json
    rm -f etc/pms/treasury-wallet.json
    rm -f etc/pms/admin-wallet.json
    rm -f pms-credentials.txt
    rm -rf docker_data/node
    rm -f docker-compose.test.yml
    
    echo -e "${GREEN}✅ Cleanup complete${NC}"
}

show_usage() {
    echo "Usage: $0 {setup|down|clean|status}"
    echo ""
    echo "Commands:"
    echo "  setup  - Generate keys, TLS, build images, start 4-service stack"
    echo "  down   - Stop the stack"
    echo "  clean  - Stop stack and remove all generated files + volumes"
    echo "  status - Show stack status"
    echo ""
    echo "Architecture (4 VPS - All Mandatory):"
    echo "  ┌────────────────┐    ┌────────────────┐"
    echo "  │ VPS 1: Gateway │───▶│ VPS 2: Engine  │"
    echo "  │ (Port 8443)    │    │ (Internal)     │"
    echo "  └────────────────┘    └───────┬────────┘"
    echo "                                │"
    echo "  ┌────────────────┐    ┌───────▼────────┐"
    echo "  │ VPS 4: Metrics │◀───│ VPS 3: RocksDB │"
    echo "  │ (Port 9091)    │    │ (Persistent)   │"
    echo "  └────────────────┘    └────────────────┘"
    echo ""
    echo "After setup, run tests with:"
    echo "  cargo test -p pms-server --test distributed_tx_e2e -- --ignored --nocapture"
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
        create_prometheus_config
        
        # Configure Treasury Address from generated wallet
        echo -e "${YELLOW}⚙️  Configuring Treasury Address...${NC}"
        TREASURY_ADDR=$(cat etc/pms/treasury-wallet.json | jq -r '.address')
        echo "   Treasury Address: $TREASURY_ADDR"

        # Update config with treasury address
        if grep -q "treasury_addresses =" etc/config/config.docker-test.toml; then
            # Replace existing treasury_addresses line
            sed -i '' "s|treasury_addresses = .*|treasury_addresses = [\"$TREASURY_ADDR\"]|" etc/config/config.docker-test.toml
        else
            # Add treasury_addresses after [fees] section
            export TREASURY_ADDR
            perl -i -pe 's/^\[fees\]$/[fees]\ntreasury_addresses = ["$ENV{TREASURY_ADDR}"]/' etc/config/config.docker-test.toml
        fi

        create_test_compose
        build_and_start

        # Register node via Gateway (the only public entry point)
        echo -e "${YELLOW}📝 Registering node (via Gateway)...${NC}"
        N_PK_PRIV=$(cat etc/pms/node.key)
        N_PUB=$(cargo run -q -p tools-cli -- priv-to-pub "$N_PK_PRIV")
        N_WALLET=$(cargo run -q -p tools-cli -- key-to-wallet "$N_PK_PRIV")
        echo "   Coordinator PubKey: ${N_PUB:0:20}..."
        echo "   Coordinator Wallet: $N_WALLET"
        
        # Register via Gateway (port 8443)
        curl -k -s -X POST -H "Content-Type: application/json" \
            -d "{\"node_pk\":\"$N_PUB\", \"api_url\":\"https://pms-engine:8080\", \"wallet_address\":\"$N_WALLET\"}" \
            https://127.0.0.1:8443/v1/register > /dev/null
        
        echo -e "${GREEN}✅ Node registered (via Gateway)${NC}"

        # Generate credentials file
        generate_credentials_file

        echo ""
        echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
        echo -e "${GREEN}✅ 4-SERVICE VPS ARCHITECTURE READY! (Single Writer Mode)${NC}"
        echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
        echo ""
        echo "Services (All Mandatory):"
        echo "  - Gateway (Public):  https://127.0.0.1:8443  ← SOLE ENTRY POINT"
        echo "  - Dashboard:         https://127.0.0.1:8443/dashboard/"
        echo "  - Engine (Internal): https://pms-engine:8080 (Docker network only)"
        echo "  - Prometheus:        http://127.0.0.1:9091"
        echo "  - RocksDB:           Docker volume 'rocksdb_data'"
        echo ""
        echo -e "${BLUE}📄 All credentials saved to: pms-credentials.txt${NC}"
        echo ""
        echo "Run tests (via Gateway):"
        echo "  cargo test -p pms-server --test distributed_tx_e2e -- --ignored --nocapture"
        ;;
    down)
        stop_cluster
        ;;
    clean)
        cleanup
        ;;
    status)
        echo -e "${BLUE}📊 Stack Status:${NC}"
        docker compose -f docker-compose.test.yml ps 2>/dev/null || echo "No stack running"
        ;;
    *)
        show_usage
        exit 1
        ;;
esac
