#!/usr/bin/env bash
set -euo pipefail

# Generate validation corpora from science-corpus.jsonl
# Creates progressively smaller test datasets for real-world validation
#
# Each line in the JSONL file is a document with multiple sentences
# We split by documents (lines) to reach approximate sentence counts

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR/.."
CORPUS="$PROJECT_ROOT/lingrep-core/cache/science-corpus.jsonl"
OUTPUT_DIR="$PROJECT_ROOT/test-caches/validation"

echo "=== Generating Validation Corpora ==="
echo "Source: $CORPUS"
echo "Output: $OUTPUT_DIR"
echo ""

# Check source corpus exists
if [ ! -f "$CORPUS" ]; then
    echo "Error: Source corpus not found at $CORPUS"
    exit 1
fi

# Get total document and sentence counts
TOTAL_DOCS=$(wc -l < "$CORPUS")
TOTAL_SENTENCES=$(python3 -c "
import json
sentence_count = 0
with open('$CORPUS', 'r') as f:
    for line in f:
        data = json.loads(line)
        if 'sentences' in data:
            sentence_count += len(data['sentences'])
print(sentence_count)
")

echo "Total documents: $TOTAL_DOCS"
echo "Total sentences: $TOTAL_SENTENCES"
echo ""

# Create output directory
mkdir -p "$OUTPUT_DIR"

# Calculate approximate document counts for each target
# We have ~1.63M sentences in ~6.3K documents
# Target: 10K, 100K, 500K, 1M sentences

# Approximate documents needed (1.63M sentences / 6371 docs = ~256 sentences/doc)
# 10K sentences ≈ 40 documents
# 100K sentences ≈ 400 documents
# 500K sentences ≈ 2000 documents
# 1M sentences ≈ 4000 documents

echo "Generating validation corpora..."

# 10K sentences (≈40 docs)
if [ $TOTAL_SENTENCES -ge 10000 ]; then
    echo "  [1/4] Generating validation-10k.jsonl (target: 10,000 sentences)..."
    head -n 40 "$CORPUS" > "$OUTPUT_DIR/validation-10k.jsonl"
    ACTUAL_SENTENCES=$(python3 -c "import json; s=sum(len(json.loads(l).get('sentences', [])) for l in open('$OUTPUT_DIR/validation-10k.jsonl')); print(s)")
    ACTUAL_DOCS=$(wc -l < "$OUTPUT_DIR/validation-10k.jsonl")
    echo "      Done: $ACTUAL_DOCS docs, $ACTUAL_SENTENCES sentences"
else
    echo "  [1/4] Skipping validation-10k.jsonl (not enough sentences)"
fi

# 100K sentences (≈400 docs)
if [ $TOTAL_SENTENCES -ge 100000 ]; then
    echo "  [2/4] Generating validation-100k.jsonl (target: 100,000 sentences)..."
    head -n 400 "$CORPUS" > "$OUTPUT_DIR/validation-100k.jsonl"
    ACTUAL_SENTENCES=$(python3 -c "import json; s=sum(len(json.loads(l).get('sentences', [])) for l in open('$OUTPUT_DIR/validation-100k.jsonl')); print(s)")
    ACTUAL_DOCS=$(wc -l < "$OUTPUT_DIR/validation-100k.jsonl")
    echo "      Done: $ACTUAL_DOCS docs, $ACTUAL_SENTENCES sentences"
else
    echo "  [2/4] Skipping validation-100k.jsonl (not enough sentences)"
fi

# 500K sentences (≈2000 docs)
if [ $TOTAL_SENTENCES -ge 500000 ]; then
    echo "  [3/4] Generating validation-500k.jsonl (target: 500,000 sentences)..."
    head -n 2000 "$CORPUS" > "$OUTPUT_DIR/validation-500k.jsonl"
    ACTUAL_SENTENCES=$(python3 -c "import json; s=sum(len(json.loads(l).get('sentences', [])) for l in open('$OUTPUT_DIR/validation-500k.jsonl')); print(s)")
    ACTUAL_DOCS=$(wc -l < "$OUTPUT_DIR/validation-500k.jsonl")
    echo "      Done: $ACTUAL_DOCS docs, $ACTUAL_SENTENCES sentences"
else
    echo "  [3/4] Skipping validation-500k.jsonl (not enough sentences)"
fi

# 1M sentences (≈4000 docs)
if [ $TOTAL_SENTENCES -ge 1000000 ]; then
    echo "  [4/4] Generating validation-1m.jsonl (target: 1,000,000 sentences)..."
    head -n 4000 "$CORPUS" > "$OUTPUT_DIR/validation-1m.jsonl"
    ACTUAL_SENTENCES=$(python3 -c "import json; s=sum(len(json.loads(l).get('sentences', [])) for l in open('$OUTPUT_DIR/validation-1m.jsonl')); print(s)")
    ACTUAL_DOCS=$(wc -l < "$OUTPUT_DIR/validation-1m.jsonl")
    echo "      Done: $ACTUAL_DOCS docs, $ACTUAL_SENTENCES sentences"
else
    echo "  [4/4] Skipping validation-1m.jsonl (not enough sentences)"
fi

echo ""
echo "=== Summary ==="
ls -lh "$OUTPUT_DIR"
echo ""
echo "✅ Validation corpora generated successfully!"
