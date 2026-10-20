#!/usr/bin/env bash
set -euo pipefail

# Generate memory profiling fixtures from science-corpus.jsonl
#
# Usage: ./scripts/generate_memory_fixtures.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
CORPUS_FILE="$PROJECT_ROOT/science-corpus.jsonl"
OUTPUT_DIR="$PROJECT_ROOT/test-caches"

# Check corpus exists
if [ ! -f "$CORPUS_FILE" ]; then
    echo "Error: $CORPUS_FILE not found"
    exit 1
fi

# Get total line count
TOTAL_LINES=$(wc -l < "$CORPUS_FILE")
echo "Total sentences in corpus: $TOTAL_LINES"

# Adjusted targets based on available corpus
# Using: 500, 2K, 5K, and full corpus
SIZES=(500 2000 5000 "$TOTAL_LINES")

mkdir -p "$OUTPUT_DIR"

echo "Generating memory profiling fixtures..."
echo ""

for SIZE in "${SIZES[@]}"; do
    # Determine output filename
    if [ "$SIZE" -eq "$TOTAL_LINES" ]; then
        OUTPUT_FILE="$OUTPUT_DIR/memory-full.jsonl"
        LABEL="full corpus ($SIZE sentences)"
    else
        OUTPUT_FILE="$OUTPUT_DIR/memory-${SIZE}.jsonl"
        LABEL="$SIZE sentences"
    fi

    echo "Generating: $LABEL"

    # Random sample using shuf
    shuf -n "$SIZE" "$CORPUS_FILE" > "$OUTPUT_FILE"

    # Verify line count
    ACTUAL=$(wc -l < "$OUTPUT_FILE")
    echo "  → $OUTPUT_FILE: $ACTUAL lines"

    # Estimate token count (rough approximation: 10 tokens per sentence)
    ESTIMATED_TOKENS=$((ACTUAL * 10))
    echo "  → Estimated tokens: ~$ESTIMATED_TOKENS"
    echo ""
done

echo "✓ Fixture generation complete!"
echo ""
echo "Generated files:"
ls -lh "$OUTPUT_DIR"/memory-*.jsonl | awk '{print "  " $9 " (" $5 ")"}'
