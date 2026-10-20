#!/usr/bin/env bash
set -euo pipefail

# Generate simple JSONL test fixtures for direct streaming
# Format: {"text": "sentence"} (one per line)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_DIR="${SCRIPT_DIR}/../test-caches"
mkdir -p "$OUTPUT_DIR"

# Sample Japanese sentences for testing
cat > "$OUTPUT_DIR/simple-200.jsonl" << 'SENTENCES'
{"text": "これはテストです。"}
{"text": "日本語の文章を処理します。"}
{"text": "形態素解析を行います。"}
{"text": "必要があることを確認しました。"}
{"text": "もしかすると、かもしれない。"}
{"text": "明日は雨でしょう。"}
{"text": "本を読むことが好きです。"}
{"text": "日本の文化について学びます。"}
{"text": "研究を行いました。"}
{"text": "結果を発表します。"}
SENTENCES

# Duplicate to create larger fixtures
echo "Creating simple-500.jsonl..."
for i in {1..5}; do
  cat "$OUTPUT_DIR/simple-200.jsonl" >> "$OUTPUT_DIR/simple-500.jsonl"
done

echo "Creating simple-1000.jsonl..."
for i in {1..10}; do
  cat "$OUTPUT_DIR/simple-200.jsonl" >> "$OUTPUT_DIR/simple-1000.jsonl"
done

echo "Creating simple-2000.jsonl..."
for i in {1..20}; do
  cat "$OUTPUT_DIR/simple-200.jsonl" >> "$OUTPUT_DIR/simple-2000.jsonl"
done

# Count lines
echo "Test fixtures created:"
wc -l "$OUTPUT_DIR"/simple-*.jsonl

