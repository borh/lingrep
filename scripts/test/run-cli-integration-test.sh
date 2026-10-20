#!/bin/bash
# CLI Integration Test Runner
#
# This script runs the retained ling integration-test surface.
#
# USAGE:
#   ./run-cli-integration-test.sh              # Run all tests
#   ./run-cli-integration-test.sh --verbose    # Run with verbose output

set -euo pipefail

echo "=== CLI Integration Test Runner ==="
echo ""

echo "Running CLI integration test..."
echo ""

cargo test -p lingrep --tests -- --nocapture

echo ""
echo "=== Test Complete ==="
