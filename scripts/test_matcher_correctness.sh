#!/usr/bin/env bash
# The matcher correctness gate: the dict-free lingrep-core tests that pin observable
# CompiledCatalog behavior against the independent oracle and the lingrep patterns contracts.
# Single source of truth — ci.sh and the docs point here. Do NOT add
# dictionary-dependent targets (cli_validation_tests, sf_experimental_lx_test).
set -euo pipefail

# Run from the workspace root regardless of the caller's cwd, so the documented
# contributor command works from anywhere inside or outside the repo.
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/.." && pwd)"
cd "$repo_root"

echo "▶ matcher correctness gate"
"$repo_root/scripts/cargo-test-selected.py" -p lingrep-core --lib fsm::compiler::oracle --quiet
"$repo_root/scripts/cargo-test-selected.py" -p lingrep-core --test suite -- matcher_contract:: --quiet
"$repo_root/scripts/cargo-test-selected.py" -p lingrep-core --test suite -- span_equality_test:: --quiet
"$repo_root/scripts/cargo-test-selected.py" -p lingrep-core --test suite -- pattern_compiler_contract:: --quiet
"$repo_root/scripts/cargo-test-selected.py" -p lingrep-core --test suite -- matcher_property_tests:: --quiet
"$repo_root/scripts/cargo-test-selected.py" -p lingrep-core --test suite -- named_groups_test:: --quiet
"$repo_root/scripts/cargo-test-selected.py" -p lingrep-core --test suite -- fsm_kernel_characterization:: --quiet
echo "✓ matcher correctness gate passed"
