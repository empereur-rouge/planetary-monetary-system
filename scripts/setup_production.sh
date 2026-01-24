#!/bin/bash
# =============================================================================
# Production Environment Setup Script
# =============================================================================
# This script generates all secrets and configurations needed for production.
# Run this ONCE before first deployment.
#
# Usage: ./scripts/setup_production.sh <DOMAIN>
# Example: ./scripts/setup_production.sh node.pms.network

set -e

DOMAIN="${1:-}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

if [ -z "$DOMAIN" ]; then
    echo "Usage: $0 <DOMAIN>"
    echo "Example: $0 node.pms.network"
    exit 1
fi

echo "🔧 Production Setup for domain: $DOMAIN"
echo ""

# =============================================================================
# 1. Generate PMS_ADMIN_TOKEN (64 characters, base64)
# =============================================================================
echo "🔑 Generating secure admin token (64+ chars)..."
ADMIN_TOKEN=$(openssl rand -base64 48 | tr -d '\n')
echo "   Token generated: ${ADMIN_TOKEN:0:8}...${ADMIN_TOKEN: -8}"

# Save to .env.production (gitignored)
cat > "$PROJECT_ROOT/.env.production" << EOF
# Production Environment Variables
# Generated: $(date -u +"%Y-%m-%dT%H:%M:%SZ")
# KEEP THIS FILE SECRET - DO NOT COMMIT

PMS_ADMIN_TOKEN=$ADMIN_TOKEN
PMS_CONFIG=/home/pms/config/config.prod.toml
EOF

echo "   ✅ Saved to .env.production"

# =============================================================================
# 2. Generate Node Identity Key
# =============================================================================
echo ""
echo "🔐 Generating node identity key..."
mkdir -p "$PROJECT_ROOT/etc/pms"

if [ ! -f "$PROJECT_ROOT/etc/pms/node-identity.key" ]; then
    openssl rand -out "$PROJECT_ROOT/etc/pms/node-identity.key" 32
    echo "   ✅ Node key generated"
else
    echo "   ⚠️  Node key already exists, skipping"
fi

# =============================================================================
# 3. TLS Certificates (Let's Encrypt via Caddy)
# =============================================================================
echo ""
echo "🔒 Configuring TLS (Let's Encrypt via Caddy)..."

cat > "$PROJECT_ROOT/Caddyfile.prod" << EOF
# Production Caddyfile with automatic Let's Encrypt TLS
# Domain: $DOMAIN

$DOMAIN {
    # Automatic HTTPS with Let's Encrypt
    tls {
        # Email for Let's Encrypt notifications (change this!)
        email admin@$DOMAIN
    }
    
    # Reverse proxy to PMS node
    reverse_proxy node1:8080 {
        header_up Host {host}
        header_up X-Real-IP {remote}
        header_up X-Forwarded-For {remote}
        header_up X-Forwarded-Proto {scheme}
    }
    
    # Security headers
    header {
        Strict-Transport-Security "max-age=31536000; includeSubDomains"
        X-Content-Type-Options "nosniff"
        X-Frame-Options "DENY"
        -Server
    }
    
    # Health check endpoint (no auth)
    handle /livez {
        reverse_proxy node1:8080
    }
    
    # Metrics (require admin IP)
    handle /metrics {
        # Uncomment and set your admin IP:
        # @admin remote_ip 1.2.3.4/32
        # reverse_proxy @admin node1:8080
        respond "Forbidden" 403
    }
}
EOF

echo "   ✅ Caddyfile.prod created"
echo "   📝 Edit email in Caddyfile.prod before deploying"

# =============================================================================
# 4. Production docker-compose
# =============================================================================
echo ""
echo "🐳 Creating docker-compose.prod.yml..."

cat > "$PROJECT_ROOT/docker-compose.prod.yml" << EOF
# Production Docker Compose
# Generated: $(date -u +"%Y-%m-%dT%H:%M:%SZ")

services:
  node1:
    build:
      context: .
      dockerfile: Dockerfile
      target: runtime
    image: pms-node:latest
    container_name: pms-node-prod
    restart: unless-stopped
    user: "pms"
    env_file:
      - .env.production
    environment:
      RUST_LOG: info,pms_server=info
      # No known_peers for single node, add here when you have more nodes:
      # PMS__P2P__KNOWN_PEERS: "p2ps://node2.example.com:8443,p2ps://node3.example.com:8443"
    volumes:
      - ./etc/config/config.prod.toml:/home/pms/config/config.prod.toml:ro
      - ./etc/pms/node-identity.key:/home/pms/config/node-identity.key:ro
      - ./etc/pms/admin-wallet.json:/home/pms/config/admin-wallet.json:ro
      - pms_data:/home/pms/data
    # Healthcheck defined in Dockerfile is sufficient
    # healthcheck:
    #   test: ["CMD-SHELL", "timeout 2 bash -c '</dev/tcp/127.0.0.1/8080' || exit 1"]
    #   interval: 30s
    #   timeout: 5s
    #   retries: 3
    # Internal only - Caddy handles external traffic
    expose:
      - "8080"

  caddy:
    image: caddy:2-alpine
    restart: unless-stopped
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - ./Caddyfile.prod:/etc/caddy/Caddyfile:ro
      - caddy_data:/data
      - caddy_config:/config
    depends_on:
      node1:
        condition: service_healthy

volumes:
  pms_data:
  caddy_data:
  caddy_config:
EOF

echo "   ✅ docker-compose.prod.yml created"

# =============================================================================
# Summary
# =============================================================================
echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "✅ Production setup complete!"
echo "═══════════════════════════════════════════════════════════════"
echo ""
echo "📁 Files created:"
echo "   - .env.production (SECRET - gitignored)"
echo "   - Caddyfile.prod"
echo "   - docker-compose.prod.yml"
echo ""
echo "🚀 To deploy:"
echo "   1. Edit Caddyfile.prod (set admin email)"
echo "   2. Copy files to your server"
echo "   3. Run: docker compose -f docker-compose.prod.yml up -d"
echo ""
echo "🔐 Your admin token starts with: ${ADMIN_TOKEN:0:8}..."
echo "   Full token is in .env.production"
echo ""
echo "⚠️  Remember to:"
echo "   - Point DNS for $DOMAIN to your server IP"
echo "   - Open ports 80 and 443 in firewall"
echo "   - Add known_peers when you have more nodes"
echo ""
