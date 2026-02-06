#!/bin/bash
# Run E2E Production Simulation Test
# Usage: ./scripts/run_e2e_prod.sh [--verbose]

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$PROJECT_ROOT"

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  PMS E2E Production Simulation Test"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Check prerequisites
echo "📋 Checking prerequisites..."

if ! command -v docker &> /dev/null; then
    echo "❌ Docker is not installed. Please install Docker first."
    exit 1
fi

if ! command -v docker compose &> /dev/null; then
    echo "❌ Docker Compose is not installed. Please install Docker Compose first."
    exit 1
fi

if ! command -v cargo &> /dev/null; then
    echo "❌ Cargo is not installed. Please install Rust toolchain first."
    exit 1
fi

echo "✅ All prerequisites satisfied"
echo ""

# Clean up any existing containers
echo "🧹 Cleaning up existing containers..."
docker compose -f docker-compose.e2e-prod.yml down -v 2>/dev/null || true
echo ""

# Set log level
RUST_LOG="info,pms_server=debug,pms_core=debug"
if [[ "$1" == "--verbose" || "$1" == "-v" ]]; then
    RUST_LOG="debug"
    echo "🔍 Running in VERBOSE mode"
    echo ""
fi

# Run the test
echo "🚀 Starting E2E Production Simulation..."
echo ""

export RUST_LOG="$RUST_LOG"
cargo test --test e2e_prod_sim -- --ignored --nocapture

TEST_EXIT_CODE=$?

echo ""
if [ $TEST_EXIT_CODE -eq 0 ]; then
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  ✅ E2E Production Simulation PASSED"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
else
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  ❌ E2E Production Simulation FAILED"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
fi

echo ""
echo "📝 For more information, see: E2E_PROD_SIMULATION.md"
echo ""

exit $TEST_EXIT_CODE
