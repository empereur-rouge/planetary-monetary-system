#!/bin/bash
# =============================================================================
# Benchmark PMS - Single Node Docker or Local Test
# =============================================================================
#
# Usage:
#   ./scripts/benchmark_local.sh                    # Local test (cargo test)
#   ./scripts/benchmark_local.sh --docker           # Docker single-node benchmark
#   ./scripts/benchmark_local.sh --docker --clean   # Docker + clean data before
#
# =============================================================================

set -e

# Configuration
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RESULTS_DIR="benchmark_results"
OUTPUT_FILE="${RESULTS_DIR}/bench_${TIMESTAMP}.jsonl"
SUMMARY_FILE="${RESULTS_DIR}/bench_${TIMESTAMP}_summary.md"

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
RED='\033[0;31m'
NC='\033[0m'

# Parse arguments
DOCKER_MODE=false
CLEAN_DATA=false

for arg in "$@"; do
    case $arg in
        --docker)
            DOCKER_MODE=true
            ;;
        --clean)
            CLEAN_DATA=true
            ;;
    esac
done

echo -e "${BLUE}╔══════════════════════════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║           PMS Benchmark - Single Node TPS Test               ║${NC}"
echo -e "${BLUE}╚══════════════════════════════════════════════════════════════╝${NC}"
echo ""

mkdir -p "$RESULTS_DIR"

if [ "$DOCKER_MODE" = true ]; then
    echo -e "${YELLOW}🐳 Docker Mode Enabled${NC}"
    echo ""

    # Clean data if requested
    if [ "$CLEAN_DATA" = true ]; then
        echo -e "${YELLOW}[1/5] Cleaning previous benchmark data...${NC}"
        rm -rf docker_data/bench 2>/dev/null || true
        mkdir -p docker_data/bench
    else
        echo -e "${YELLOW}[1/5] Keeping existing data (use --clean to reset)${NC}"
    fi

    # Build and start Docker
    echo -e "${YELLOW}[2/5] Starting Docker benchmark node...${NC}"
    docker compose -f docker-compose.bench.yml down 2>/dev/null || true
    docker compose -f docker-compose.bench.yml up -d --build

    # Wait for node to be healthy
    echo -e "${YELLOW}[3/5] Waiting for node to be ready...${NC}"
    ATTEMPTS=0
    until docker compose -f docker-compose.bench.yml ps | grep -q "healthy"; do
        ATTEMPTS=$((ATTEMPTS + 1))
        if [ $ATTEMPTS -gt 60 ]; then
            echo -e "${RED}❌ Node failed to start after 60 attempts${NC}"
            docker compose -f docker-compose.bench.yml logs --tail=50
            docker compose -f docker-compose.bench.yml down
            exit 1
        fi
        echo "   Waiting... ($ATTEMPTS/60)"
        sleep 2
    done
    echo -e "${GREEN}✅ Node is healthy${NC}"

    # Run benchmark test
    echo -e "${YELLOW}[4/5] Running benchmark (10,000 transactions)...${NC}"
    echo ""
    
    RUST_LOG="warn,pms_bench=info" \
        cargo test --release -p pms-server docker_bench_single -- --nocapture 2>&1 | \
        tee >(grep '"target":"pms_bench"' > "$OUTPUT_FILE" 2>/dev/null || true) | \
        grep -E "(Progress|BENCHMARK|TPS|═|╔|╚|║|✅|❌|🚀|📊)" || true

    echo ""
    echo -e "${YELLOW}[5/5] Collecting metrics...${NC}"

    # Get node logs for additional metrics
    docker compose -f docker-compose.bench.yml logs --since 5m 2>/dev/null | \
        grep '"target":"pms_bench"' >> "$OUTPUT_FILE" 2>/dev/null || true

    # Analyze results
    BLOCKS_VALIDATED=$(grep -c '"event":"block_validated"' "$OUTPUT_FILE" 2>/dev/null || echo "0")
    BLOCKS_REJECTED=$(grep -c '"event":"block_rejected"' "$OUTPUT_FILE" 2>/dev/null || echo "0")
    BLOCKS_ORPHANED=$(grep -c '"event":"block_orphaned"' "$OUTPUT_FILE" 2>/dev/null || echo "0")

    # Get benchmark summary from test output
    BENCHMARK_TPS=$(grep '"event":"benchmark_complete"' "$OUTPUT_FILE" 2>/dev/null | grep -oE '"tps":[0-9.]+' | cut -d: -f2 | head -1 || echo "N/A")

    # Generate report
    cat > "$SUMMARY_FILE" << EOF
# Benchmark Docker Single-Node - ${TIMESTAMP}

## Configuration
- **Mode**: Docker Single Node
- **Transactions**: 10,000
- **Build**: Release

## Results

| Metric | Value |
|--------|-------|
| Blocks validated | ${BLOCKS_VALIDATED} |
| Blocks rejected | ${BLOCKS_REJECTED} |
| Blocks orphaned | ${BLOCKS_ORPHANED} |
| **TPS** | **${BENCHMARK_TPS}** |

## Files
- Logs: \`${OUTPUT_FILE}\`
- Report: \`${SUMMARY_FILE}\`

## Commands

\`\`\`bash
# View summary
cat ${SUMMARY_FILE}

# Analyze persist times
grep block_validated ${OUTPUT_FILE} | jq -s 'map(.persist_ms) | add/length'

# Stop container
docker compose -f docker-compose.bench.yml down
\`\`\`
EOF

    # Display summary
    echo ""
    echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}                    FINAL SUMMARY                              ${NC}"
    echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "  Blocks validated: ${GREEN}${BLOCKS_VALIDATED}${NC}"
    echo -e "  Blocks rejected:  ${BLOCKS_REJECTED}"
    echo -e "  Blocks orphaned:  ${BLOCKS_ORPHANED}"
    echo -e "  ${GREEN}TPS: ${BENCHMARK_TPS}${NC}"
    echo ""
    echo -e "  Report: ${BLUE}${SUMMARY_FILE}${NC}"
    echo ""
    echo -e "${YELLOW}💡 Container still running. Run 'docker compose -f docker-compose.bench.yml down' to stop.${NC}"

else
    # Local test mode (original behavior)
    DURATION=${1:-60}
    echo -e "${YELLOW}📦 Local Test Mode (${DURATION}s)${NC}"
    echo ""
    
    echo -e "${YELLOW}[1/4] Compiling in release mode...${NC}"
    cargo build --release -p pms-server 2>&1 | tail -3

    echo -e "${YELLOW}[2/4] Running stress test...${NC}"
    RUST_LOG="warn,pms_bench=info" \
        timeout "${DURATION}s" cargo test --release -p pms-server docker_stress_sync -- --nocapture 2>&1 | \
        tee >(grep '"target":"pms_bench"' > "$OUTPUT_FILE" 2>/dev/null || true) | \
        grep -E "(📊|TPS|blocks)" | head -50 || true

    echo -e "${YELLOW}[3/4] Analyzing results...${NC}"
    BLOCKS_VALIDATED=$(grep -c '"event":"block_validated"' "$OUTPUT_FILE" 2>/dev/null || echo "0")
    
    echo ""
    echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "  Blocks validated: ${GREEN}${BLOCKS_VALIDATED}${NC}"
    echo -e "  Logs: ${BLUE}${OUTPUT_FILE}${NC}"
    echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
fi
