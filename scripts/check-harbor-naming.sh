#!/usr/bin/env bash
# scripts/check-harbor-naming.sh — fail if the Harbor harness still names the
# pre-rename product.
#
# `benchmarks/harbor` is a benchmark of the `ling`/`lingrep` surface, and it is
# read as evidence rather than as code: its README is the protocol description,
# its Python emits the schemas, and its Nix images carry the arm names. A
# partial rename is therefore not cosmetic. It cost a whole review round to
# notice that `README.md` still described `VP` trials and a `lx_queries` field
# the code had already renamed, because nothing runs this tree's prose.
#
# The retired names are:
#   VP        the product, and the evaluation's own arm and task names
#   vp-search the retired binary
#   SGX       the retired pattern-language name
#
# They may still appear in `jobs/` and `.generated/`, which are recorded
# experiment output: a manifest or trajectory from a past run is evidence of
# what that run did, and rewriting it would falsify the record. Both are
# gitignored, so this scan skips them by exclusion rather than by trust.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

readonly root=benchmarks/harbor
readonly retired='\bVP\b|\bvp\b|vp-search|\bSGX\b|\bsgx\b'

status=0

# Shell-checked list of what the harness ships: prose, Python, the launcher, the
# catalogs, and the Nix half of the images.
mapfile -t files < <(
    find "$root" \
        \( -path "$root/jobs" -o -path "$root/.generated" -o -path "$root/.venv" \
           -o -name '__pycache__' -o -name '.pytest_cache' -o -name '.ruff_cache' \) -prune \
        -o -type f \
        \( -name '*.md' -o -name '*.py' -o -name '*.rs' -o -name '*.json' \
           -o -name '*.toml' -o -name 'docker' \) -print \
        | sort
)

if [ "${#files[@]}" -eq 0 ]; then
    echo "error: no Harbor harness files found under $root — is the path out of date?" >&2
    exit 1
fi

while IFS= read -r hit; do
    status=1
    echo "retired name in the Harbor harness: $hit" >&2
done < <(grep -nE "$retired" "${files[@]}" || true)

# The launcher is embedded in a Nix image and its Rust tests run inside that
# build, so a rename that breaks it is caught there. Prose is not, which is the
# half this check exists for.
readonly -a prose=(
    "$root/README.md"
    "$root/guides/analyzer-guide.md"
    "$root/guides/annotation-guide.md"
)
for path in "${prose[@]}"; do
    if [ ! -f "$path" ]; then
        echo "error: $path does not exist — is the prose list out of date?" >&2
        status=1
    fi
done

# The report emits these summary keys; the README describes them by name. A
# rename on one side and not the other is how `lx_queries` drifted: the code said
# `pattern_queries` and the protocol description said something that never
# existed. Both halves must agree, in this tree, at review time.
for field in pattern_catalog_uses pattern_lifecycle pattern_queries; do
    if ! grep -q "\"$field\"" "$root/command_analysis.py" "$root/report.py"; then
        echo "error: report field $field is not emitted by the harness" >&2
        status=1
        continue
    fi
    if ! grep -q "$field" "$root/README.md"; then
        echo "error: $root/README.md does not describe the emitted field $field" >&2
        status=1
    fi
done

if [ "$status" -eq 0 ]; then
    echo "✓ Harbor harness names the current product, arms and report fields"
fi

exit "$status"
