#!/usr/bin/env bash
set -euo pipefail

repo_root=${LINGREP_REPO_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)}
project="$repo_root/python/spacy-adapter"
mode=${1:-full}
run_rocm=${LINGREP_SPACY_RUN_ROCM:-0}
run_cuda=${LINGREP_SPACY_RUN_CUDA:-0}
python_cache=$(mktemp -d)
trap 'rm -rf -- "$python_cache"' EXIT
export PYTHONPYCACHEPREFIX=$python_cache

uv lock --project "$project" --check
cargo test -p lingrep-core --example measure_spacy_process

case "$mode" in
  static)
    uv run --project "$project" --frozen python -m compileall -q \
      "$project/src" "$repo_root/python/adapter-protocol/src" "$project/tests"
    uv run --project "$project" --frozen python -m unittest discover \
      -s "$project/tests" -v
    ;;
  full)
    LINGREP_SPACY_TEST_REAL=1 LINGREP_SPACY_RUN_ROCM=0 \
      LINGREP_SPACY_RUN_CUDA=0 LINGREP_SPACY_GINZA_MODEL= \
      uv run --project "$project" --frozen \
      --extra cpu --extra spacy \
      pytest "$project/tests"
    adapter_python=$(uv run --project "$project" --frozen \
      --extra cpu --extra spacy python -c 'import sys; print(sys.executable)')
    LINGREP_SPACY_ADAPTER_PYTHON=$adapter_python \
      "$repo_root/scripts/cargo-test-selected.py" -p lingrep-core --test external_process_contract \
      locked_spacy_adapter_runs_through_the_verified_rust_process_owner \
      -- --exact --ignored
    uv run --project "$project" --frozen ruff check \
      "$project/src" "$repo_root/python/adapter-protocol/src" "$project/tests" \
      "$project/bin/lingrep-spacy-adapter" "$project/bin/ginza-gpu"

    ginza_model=${LINGREP_SPACY_GINZA_MODEL:-ja_ginza}
    model_extra=${LINGREP_SPACY_MODEL_EXTRA:-ginza}
    if [[ -n $ginza_model ]]; then
      case "$model_extra" in
        ginza|electra) ;;
        *)
          echo "LINGREP_SPACY_MODEL_EXTRA must be ginza or electra" >&2
          exit 2
          ;;
      esac
      LINGREP_SPACY_GINZA_MODEL=$ginza_model \
        uv run --project "$project" --frozen --extra cpu --extra spacy \
        --extra "$model_extra" pytest "$project/tests" \
        -k real_ginza_model
      adapter_python=$(uv run --project "$project" --frozen \
        --extra cpu --extra spacy --extra "$model_extra" \
        python -c 'import sys; print(sys.executable)')
      LINGREP_SPACY_GINZA_MODEL=$ginza_model LINGREP_SPACY_ADAPTER_PYTHON=$adapter_python \
        "$repo_root/scripts/cargo-test-selected.py" -p lingrep-core --test external_process_contract \
        locked_ginza_model_runs_through_rust_with_validated_identity_and_provenance \
        -- --exact --ignored
    fi

    if [[ $run_rocm == 1 ]]; then
      if [[ -z ${LINGREP_SPACY_GINZA_MODEL:-} ]]; then
        echo "LINGREP_SPACY_RUN_ROCM=1 requires LINGREP_SPACY_GINZA_MODEL" >&2
        exit 2
      fi
      LINGREP_SPACY_RUN_CUDA=0 \
        uv run --project "$project" --frozen --extra rocm --extra spacy \
        --extra "${LINGREP_SPACY_MODEL_EXTRA:-ginza}" pytest "$project/tests" \
        -k 'cpu_and_rocm_processes_emit_identical_ginza_sentences or ginza_gpu_launcher'
    fi

    if [[ $run_cuda == 1 ]]; then
      LINGREP_SPACY_RUN_ROCM=0 \
        uv run --project "$project" --frozen --extra cuda --extra spacy \
        --extra "$model_extra" pytest "$project/tests" -k ginza_gpu_launcher
    fi
    ;;
  *)
    echo "usage: $0 [static|full]" >&2
    exit 2
    ;;
esac
