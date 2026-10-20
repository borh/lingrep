#!/usr/bin/env bash
#
# Memory profiling script for lingrep cache operations
#
# Usage: ./scripts/profile_memory.sh [benchmark_name] [--quick]
#
# This script uses valgrind/massif to profile memory usage of cache operations,
# providing accurate measurements (not estimates).
#
# Requirements:
# - valgrind (massif tool)
# - ms_print (comes with valgrind)
#
# Output:
# - target/profiling/memory/<benchmark_name>.massif
# - target/profiling/memory/<benchmark_name>.txt (human-readable)

set -euo pipefail

BENCHMARK="${1:-fsm_performance}"
QUICK_MODE=""
OUTPUT_DIR="target/profiling/memory"
BINARY_PATTERN="target/release/deps/${BENCHMARK}-*"

# Check for --quick flag
if [ "${2:-}" == "--quick" ]; then
    QUICK_MODE="-- --test"
    echo "Quick mode: Running subset of benchmark tests"
fi

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}=== Memory Profiling: ${BENCHMARK} ===${NC}"
echo "Output directory: ${OUTPUT_DIR}"
echo ""

# Check if valgrind is available
if ! command -v valgrind &> /dev/null; then
    echo -e "${RED}Error: valgrind not found${NC}"
    echo "Install with: nix-shell -p valgrind"
    exit 1
fi

# Create output directory
mkdir -p "${OUTPUT_DIR}"

# Build benchmark in release mode first
echo -e "${YELLOW}1. Building benchmark...${NC}"
cargo build --release --bench "${BENCHMARK}"

# Find the actual binary (with hash suffix)
BINARY=$(ls ${BINARY_PATTERN} 2>/dev/null | grep -v "\.d$" | head -1)

# Check if benchmark binary exists
if [ ! -f "${BINARY}" ]; then
    echo -e "${RED}Error: Benchmark binary not found for ${BENCHMARK}${NC}"
    echo "Available benchmarks:"
    ls lingrep-core/benches/*.rs 2>/dev/null | sed 's|lingrep-core/benches/||' | sed 's|.rs$||' || echo "  (none found)"
    exit 1
fi

echo "Found binary: ${BINARY}"

# Run valgrind massif profiling
echo ""
echo -e "${YELLOW}2. Running valgrind massif...${NC}"
echo "Note: This will take significantly longer than normal benchmarks (10-50x slower)"
echo ""

valgrind --tool=massif \
    --massif-out-file="${OUTPUT_DIR}/${BENCHMARK}.massif" \
    --time-unit=ms \
    --stacks=yes \
    --depth=100 \
    "${BINARY}" \
    ${QUICK_MODE} \
    2>&1 | tee /dev/tty | grep -E "(Massif|running|test result)" || true

# Check if massif file was created
if [ ! -f "${OUTPUT_DIR}/${BENCHMARK}.massif" ]; then
    echo -e "${RED}Error: Massif file not created${NC}"
    exit 1
fi

# Convert to human-readable format
echo ""
echo -e "${YELLOW}3. Converting massif output to text format...${NC}"
ms_print "${OUTPUT_DIR}/${BENCHMARK}.massif" > "${OUTPUT_DIR}/${BENCHMARK}.txt"

# Extract peak memory usage
PEAK_MEMORY=$(grep "mem_heap_B" "${OUTPUT_DIR}/${BENCHMARK}.massif" | grep -v "^#" | awk '{print $2}' | sort -n | tail -1)

# Convert to MB for display
if [ -n "${PEAK_MEMORY}" ]; then
    PEAK_MB=$(echo "scale=2; ${PEAK_MEMORY} / 1024 / 1024" | bc)
    PEAK_DISPLAY="${PEAK_MEMORY} bytes (${PEAK_MB} MB)"
else
    PEAK_DISPLAY="N/A"
fi

echo ""
echo -e "${GREEN}=== Profiling complete ===${NC}"
echo ""
echo "Results:"
echo "  Massif file: ${OUTPUT_DIR}/${BENCHMARK}.massif"
echo "  Text output: ${OUTPUT_DIR}/${BENCHMARK}.txt"
echo ""
echo -e "Peak heap memory: ${PEAK_DISPLAY}"
echo ""

# Display summary
if [ -f "${OUTPUT_DIR}/${BENCHMARK}.txt" ]; then
    echo "Memory usage snapshot (peak and surroundings):"
    echo ""
    # Show peak memory usage section
    grep -A 5 -B 5 "mem_heap_B" "${OUTPUT_DIR}/${BENCHMARK}.txt" | grep -E "(^=|mem_heap_B|Detailed snapshots)" | head -30
fi

echo ""
echo -e "${YELLOW}To view full results:${NC}"
echo "  cat ${OUTPUT_DIR}/${BENCHMARK}.txt"
echo "  less ${OUTPUT_DIR}/${BENCHMARK}.txt"
echo ""
echo -e "${YELLOW}To compare multiple runs:${NC}"
echo "  ms_print ${OUTPUT_DIR}/${BENCHMARK}.massif | less"
echo ""
