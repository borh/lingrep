#!/usr/bin/env bash
# Repository validation entry point.
#
#   ./ci.sh               comprehensive correctness
#   ./ci.sh --full        comprehensive correctness
#   ./ci.sh --fast        routine local/pre-push correctness
#   ./ci.sh --fuzz        bounded fuzz smoke only
#   ./ci.sh --performance retained benchmarks and real-corpus measurement only

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$repo_root"

usage() {
    echo "usage: $0 [--fast|--full|--fuzz|--performance]" >&2
}

if [[ $# -gt 1 ]]; then
    usage
    exit 2
fi

case "${1:-}" in
    "" | --full) mode=full ;;
    --fast) mode=fast ;;
    --fuzz) mode=fuzz ;;
    --performance) mode=performance ;;
    *)
        usage
        exit 2
        ;;
esac

run_environment() {
    scripts/check-build-env.sh
}

run_repository_guards() {
    local metadata
    metadata=$(cargo metadata --no-deps --format-version=1)
    if jq -e '.packages[] | select(.name == "vp-sfe")' <<<"$metadata" >/dev/null; then
        echo "vp-sfe must not remain a workspace package" >&2
        exit 1
    fi

    scripts/check-test-consolidation.sh
    scripts/check-doc-test-references.py
    scripts/check-test-thread-default.sh
    scripts/check-ling-test-launches.sh
    scripts/check-lx-catalogs.sh
    scripts/check-artifact-naming.sh
    scripts/check-durability-fixtures.sh
    scripts/check-fs-capability-seam.sh
    scripts/check-i18n-keys.py
    scripts/check-release-notices.sh
}

run_common_checks() {
    run_environment
    run_repository_guards

    echo "Checking CI and fuzz runner contracts"
    scripts/test/test_ci_modes.sh
    scripts/test/test_fuzz_smoke.sh

    echo "Checking the Python bindings lockfile"
    uv lock --project python/lingrep --check

    echo "Checking the locked spaCy adapter contract"
    scripts/check-spacy-adapter.sh static

    if [[ $mode == fast ]]; then
        echo "Checking the locked Stanza adapter contract"
        scripts/check-stanza-adapter.sh static
    fi

    echo "Checking the Hugging Face dataset adapter"
    uv lock --check --script scripts/huggingface_dataset.py
    python -m unittest scripts/test/test_huggingface_dataset.py scripts/test/test_wo_predicate_export.py \
        scripts/test/test_test_reference_guards.py scripts/test/test_fs_capability_seam_guard.py \
        scripts/test/test_lx_catalog_guard.py \
        scripts/test/test_cargo_test_selected.py scripts/test/test_web_file_limit.py \
        scripts/test/test_corpus_pressure.py scripts/test/test_release_notices.py

    if ! command -v python3 >/dev/null; then
        echo "python3 is required for performance campaign tests" >&2
        exit 2
    fi
    echo "Checking performance campaign measurements and warm-ups"
    cargo build -p lingrep-perf --bin lingrep-perf --quiet
    local perf_target=${CARGO_TARGET_DIR:-target}
    [[ $perf_target = /* ]] || perf_target="$repo_root/$perf_target"
    LINGREP_PERF_TEST_BIN="$perf_target/debug/lingrep-perf" \
        python3 -m unittest scripts/test/test_perf_measure.py

    echo "Checking the release installer"
    scripts/test/test_install.sh

    echo "Building and checking release adapter wheels"
    UV_OFFLINE=1 UV_EXCLUDE_NEWER=P3D python scripts/check-adapter-wheels.py >/dev/null 2>&1 ||
        UV_EXCLUDE_NEWER=P3D python scripts/check-adapter-wheels.py

    echo "Checking formatting"
    cargo fmt --check --all 2>&1 | tee "${TMPDIR:-/tmp}/cargo-fmt.log"

    echo "Running clippy on all targets and features"
    cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tee "${TMPDIR:-/tmp}/cargo-clippy.log"

    echo "Checking ling without the web feature"
    cargo clippy -p lingrep --all-targets --no-default-features -- -D warnings 2>&1 | tee "${TMPDIR:-/tmp}/cargo-clippy-no-web.log"
}

run_shared_tests() {
    echo "Running lingrep-web JavaScript tests"
    python3 lingrep-web/question-ui/build.py --check
    python3 lingrep-web/question-ui/build_test.py
    node --test lingrep-web/js-tests/*.test.mjs lingrep-web/question-ui/*.test.mjs

    echo "Running workspace doctests"
    cargo test --workspace --doc 2>&1 | tee "${TMPDIR:-/tmp}/cargo-test-doc.log"
}

run_fast_tests() {
    echo "Running workspace library and binary unit tests"
    cargo test --config .cargo/test.toml --workspace --lib --bins --example measure_spacy_process --quiet 2>&1 |
        tee "${TMPDIR:-/tmp}/cargo-test-fast.log"
}

run_full_tests_and_extras() {
    echo "Running complete workspace Rust test inventory"
    cargo test --config .cargo/test.toml --workspace --tests --example measure_spacy_process \
        --bench prepared_tokens --quiet 2>&1 | tee "${TMPDIR:-/tmp}/cargo-test-full.log"

    echo "Checking the DuckDB examples against the native measures"
    local ling_target=${CARGO_TARGET_DIR:-target}
    [[ $ling_target = /* ]] || ling_target="$repo_root/$ling_target"
    LING="$ling_target/debug/ling" python3 -m unittest scripts/test/test_duckdb_examples.py

    echo "Running real native portable-catalog realizations"
    scripts/cargo-test-selected.py --config .cargo/test.toml --workspace --test suite -- \
        portable_catalog_realization::real_vibrato_and_sudachi_realize_the_same_portable_catalog \
        --exact --ignored
    scripts/cargo-test-selected.py --config .cargo/test.toml --workspace --test suite -- \
        direct_protocol::sudachi_portable_catalog_publishes_nonzero_context_without_unidic_fields \
        --exact --ignored
    scripts/cargo-test-selected.py --config .cargo/test.toml --workspace --test suite -- sudachi_contract:: --ignored

    echo "Checking the installed Python bindings with Vibrato and ling"
    local bindings=(uv run --project python/lingrep --isolated --locked --all-extras --no-editable python -m)
    PATH="$ling_target/debug:$PATH" \
        "${bindings[@]}" unittest discover -s python/lingrep/tests
    "${bindings[@]}" mypy --config-file python/lingrep/pyproject.toml \
        python/lingrep/python/lingrep python/lingrep/tests/typing_usage.py \
        python/lingrep/tests/typing_polars.py

    echo "Running the locked spaCy adapter suite"
    scripts/check-spacy-adapter.sh full

    echo "Running the locked Stanza adapter suite"
    scripts/check-stanza-adapter.sh full

    uv run --project python/spacy-adapter --frozen --extra cpu --extra spacy --with ./python/lingrep \
        python -m unittest discover -s python/lingrep/tests -p test_interop.py -k spacy
    uv run --project python/stanza-adapter --frozen --extra cpu --with ./python/lingrep \
        python -m unittest discover -s python/lingrep/tests -p test_interop.py -k stanza

    run_shared_tests

    if [[ $(uname -s) == Darwin ]]; then
        if ! command -v chromedriver >/dev/null; then
            echo "full CI requires chromedriver" >&2
            exit 2
        fi
    elif ! command -v chromium >/dev/null || ! command -v chromedriver >/dev/null; then
        echo "full CI requires chromium and chromedriver" >&2
        exit 2
    fi
    echo "Running lingrep-web browser layout checks"
    scripts/run_lingrep_web_layout_e2e.sh

    # The orchestrator's own refusals and its ABBA balancing. No timing — this
    # runs in about a second and never measures anything, which is what makes
    # it admissible here at all. Without it the shell half of the tooling is
    # the only part with no lane, and it is the part that rots.
    echo "Running the performance campaign orchestrator self-test"
    scripts/test/test_perf_campaign.sh

    echo "Checking documented launcher targets"
    scripts/check-live-launchers.py
}

run_screen_lane() {
    echo "Running lingrep-perf with the counted-work screen enabled"
    cargo test -p lingrep-perf --features screen --quiet 2>&1 | tee "${TMPDIR:-/tmp}/cargo-test-lingrep-perf.log"

    local core_tests=(scripts/cargo-test-selected.py -p lingrep-core --features testing,bench-instrumentation --lib)
    "${core_tests[@]}" -- ::instrumentation_ --quiet 2>&1 |
        tee "${TMPDIR:-/tmp}/cargo-test-lingrep-core-instrumentation.log"
    "${core_tests[@]}" -- fsm::compiler::published_baseline::repository_catalog_published_records --exact --quiet 2>&1 |
        tee -a "${TMPDIR:-/tmp}/cargo-test-lingrep-core-instrumentation.log"
}

run_doc_lane() {
    echo "Building documentation with warnings denied"
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps 2>&1 | tee "${TMPDIR:-/tmp}/cargo-doc.log"

    echo "Building feature-gated documentation with warnings denied"
    RUSTDOCFLAGS="-D warnings" cargo doc -p lingrep-perf --features screen --no-deps 2>&1 |
        tee "${TMPDIR:-/tmp}/cargo-doc-lingrep-perf.log"
}

run_fuzz() {
    run_environment
    if ! command -v cargo-fuzz >/dev/null; then
        echo "fuzz CI requires cargo-fuzz" >&2
        exit 2
    fi
    scripts/run_fuzz_smoke.sh
}

run_performance() {
    run_environment
    if [[ -z ${LINGREP_REAL_CORPUS_PATH:-} || ! -f ${LINGREP_REAL_CORPUS_PATH:-} ]]; then
        echo "performance CI requires LINGREP_REAL_CORPUS_PATH naming a file" >&2
        exit 2
    fi
    if [[ -z ${LINGREP_REAL_PATTERNS_PATH:-} || ! -f ${LINGREP_REAL_PATTERNS_PATH:-} ]]; then
        echo "performance CI requires LINGREP_REAL_PATTERNS_PATH naming a file" >&2
        exit 2
    fi

    cargo bench -p lingrep-corpus --bench prepared_tokens
    cargo bench -p lingrep-core --bench fsm_performance
    cargo bench -p lingrep-core --bench corpus_discovery
    LINGREP_RUN_REAL_SENTENCE_FINAL_CLI_BENCH=1 scripts/measure_sentence_final_profile.sh
}

echo "Lingrep CI: $mode"
case "$mode" in
    fast)
        run_common_checks
        run_fast_tests
        run_shared_tests
        run_screen_lane
        run_doc_lane
        ;;
    full)
        run_common_checks
        run_full_tests_and_extras
        run_screen_lane
        run_doc_lane
        ;;
    fuzz) run_fuzz ;;
    performance) run_performance ;;
esac

echo "CI lane passed: $mode"
