#!/bin/bash
# ═══════════════════════════════════════════════════════════════════════════════
# generate_treasury_wallets.sh - Generate Treasury Wallets with Real PMS Addresses
# ═══════════════════════════════════════════════════════════════════════════════
#
# Creates 3 treasury wallets using the PMS wallet library (real bech32m addresses)
#
# Usage:
#   ./scripts/generate_treasury_wallets.sh [num_wallets] [output_dir] [json_path]
#
# Default: 3 wallets, etc/pms/treasury-keys, etc/pms/treasury-wallets.json
#
# ⚠️ IMPORTANT: Save the mnemonic phrases in a secure location!
#
# ═══════════════════════════════════════════════════════════════════════════════

set -e
cd "$(dirname "$0")/.."

NUM_WALLETS="${1:-3}"
OUTPUT_DIR="${2:-etc/pms/treasury-keys}"
JSON_PATH="${3:-etc/pms/treasury-wallets.json}"

echo "═══════════════════════════════════════════════════════════════"
echo "🏦 Treasury Wallet Generator"
echo "═══════════════════════════════════════════════════════════════"
echo ""

# Run the Rust command
cargo run -p tools-cli --release -- treasury-generate "$NUM_WALLETS" "$OUTPUT_DIR" "$JSON_PATH"

echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "✅ Generation complete!"
echo ""
echo "Next step - sign with coordinator key:"
echo "  cargo run -p tools-cli --release -- treasury-sign etc/pms/node1.key $JSON_PATH"
echo "═══════════════════════════════════════════════════════════════"
