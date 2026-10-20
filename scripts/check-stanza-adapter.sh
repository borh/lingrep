#!/usr/bin/env bash
set -euo pipefail

repo_root=${LINGREP_REPO_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)}
project="$repo_root/python/stanza-adapter"
mode=${1:-full}
python_cache=$(mktemp -d)
model_stage=
trap 'rm -rf -- "$python_cache" ${model_stage:+"$model_stage"}' EXIT
export PYTHONPYCACHEPREFIX=$python_cache

uv lock --project "$project" --check

# stanza depends on torch directly, so there is no torch-free environment to
# run these in and the no-extra resolution picks a variant on its own -- it
# chose +rocm7.2, which installs on linux/x86_64 and has no wheel anywhere
# else. Naming the extra makes the choice the script's rather than a
# resolver tiebreak, and these checks are model-free: which torch is present
# is exactly what they do not test.
run_model_free_checks() {
    uv run --project "$project" --frozen --extra cpu python -m compileall -q \
        "$project/src" "$project/tests" "$project/tools"
    uv run --project "$project" --frozen --extra cpu python -m unittest discover \
        -s "$project/tests" -v
    uv run --project "$project" --frozen --extra cpu ruff check \
        "$project/src" "$project/tests" "$project/tools" \
        "$project/bin/lingrep-stanza-adapter"
}

case "$mode" in
  static)
    run_model_free_checks
    ;;
  full)
    # The model-free unittest run that stood here is gone with the base
    # environment it distinguished: it re-ran the same discovery under the cpu
    # extra, which is now what run_model_free_checks itself uses. Same command,
    # same interpreter, immediately after -- coverage is unchanged.
    run_model_free_checks

    model_output=$(nix build "$repo_root#stanza-model-fr-gsd-morphology" \
        --no-link --print-out-paths)
    model_stage=$(mktemp -d)
    model_root="$model_stage/model"
    cp -R "$model_output/share/stanza/fr-gsd-morphology" "$model_root"
    chmod -R u+w "$model_root"
    cp "$model_root/lingrep-stanza-model.json" "$model_stage/declared-profile.json"
    uv run --project "$project" --frozen --extra cpu \
        "$project/bin/lingrep-stanza-adapter" \
        --resource-root "$model_root" \
        --language fr \
        --backend cpu \
        --prepare \
        --probe-text 'Je mange du pain.' \
        --package tokenize=gsd \
        --package mwt=gsd \
        --package pos=gsd_nocharlm \
        --package lemma=gsd_nocharlm
    cmp "$model_stage/declared-profile.json" "$model_root/lingrep-stanza-model.json"

    adapter_python=$(uv run --project "$project" --frozen --extra cpu \
        python -c 'import sys; print(sys.executable)')
    LINGREP_STANZA_ADAPTER_PYTHON=$adapter_python \
      LINGREP_STANZA_MODEL_ROOT=$model_root \
      cargo test -p lingrep-core --test external_process_contract stanza_ \
        -- --ignored --nocapture
    ;;
  *)
    echo "usage: $0 [static|full]" >&2
    exit 2
    ;;
esac
