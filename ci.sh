#!/usr/bin/env bash
# Repository validation entry point.
#
#   ./ci.sh               comprehensive correctness
#   ./ci.sh --full        comprehensive correctness
#   ./ci.sh --fast        routine local/pre-push correctness
#   ./ci.sh --quick       deprecated alias for --full
#   ./ci.sh --fuzz        bounded fuzz smoke only
#   ./ci.sh --performance retained benchmarks and real-corpus measurement only

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$repo_root"

usage() {
    echo "usage: $0 [--fast|--full|--quick|--fuzz|--performance]" >&2
}

if [[ $# -gt 1 ]]; then
    usage
    exit 2
fi

case "${1:-}" in
    "" | --full) mode=full ;;
    --fast) mode=fast ;;
    --quick)
        echo "warning: --quick is deprecated; use --full" >&2
        mode=full
        ;;
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
    scripts/check-test-filters.sh
    scripts/check-test-thread-default.sh
    scripts/check-ling-test-launches.sh
    scripts/check-lx-catalogs.sh
    scripts/check-artifact-naming.sh
    scripts/check-durability-fixtures.sh
    scripts/check-i18n-keys.py
}

run_common_checks() {
    run_environment
    run_repository_guards

    echo "Checking the locked spaCy adapter contract"
    scripts/check-spacy-adapter.sh static

    echo "Checking the locked Stanza adapter contract"
    scripts/check-stanza-adapter.sh static

    echo "Checking the Hugging Face dataset adapter"
    uv lock --check --script scripts/huggingface_dataset.py
    python -m unittest scripts/test/test_huggingface_dataset.py

    echo "Checking the release installer"
    scripts/test/test_install.sh

    echo "Checking formatting"
    cargo fmt --check --all 2>&1 | tee /tmp/cargo-fmt.log

    echo "Running clippy on all targets"
    cargo clippy --all-targets -- -D warnings 2>&1 | tee /tmp/cargo-clippy.log
}

run_shared_tests() {
    echo "Running lingrep-web JavaScript tests"
    node --test lingrep-web/js-tests/*.test.mjs

    echo "Running workspace doctests"
    cargo test --workspace --doc 2>&1 | tee /tmp/cargo-test-doc.log
}

run_fast_tests() {
    echo "Running workspace library and binary unit tests"
    cargo test --workspace --lib --bins --quiet 2>&1 | tee /tmp/cargo-test-fast.log
}

run_full_tests_and_extras() {
    echo "Running complete workspace Rust test inventory"
    cargo test --workspace --tests --quiet 2>&1 | tee /tmp/cargo-test-full.log

    echo "Running real native portable-catalog realizations"
    cargo test -p lingrep-core --test suite -- \
        portable_catalog_realization::real_vibrato_and_sudachi_realize_the_same_portable_catalog \
        --exact --ignored
    cargo test -p lingrep --test suite -- \
        direct_protocol::sudachi_portable_catalog_publishes_nonzero_context_without_unidic_fields \
        --exact --ignored

    echo "Running the locked spaCy adapter suite"
    scripts/check-spacy-adapter.sh full

    echo "Running the locked Stanza adapter suite"
    scripts/check-stanza-adapter.sh full

    run_shared_tests

    if ! command -v chromium >/dev/null || ! command -v chromedriver >/dev/null; then
        echo "full CI requires chromium and chromedriver" >&2
        exit 2
    fi
    echo "Running lingrep-web browser layout checks"
    scripts/run_lingrep_web_layout_e2e.sh

    # lingrep-perf's screen is behind a feature because taking `lingrep-core` with
    # `bench-instrumentation` unconditionally would unify that feature into the
    # shipping binaries. Feature-gated code is invisible to `cargo clippy
    # --all-targets`, which selects targets and not features, so without this
    # step the screen would be neither compiled nor linted by any lane. Tests
    # run in `run_screen_lane`; feature-specific linting stays here.
    cargo clippy -p lingrep-perf --features screen --all-targets -- -D warnings

    # The orchestrator's own refusals and its ABBA balancing. No timing — this
    # runs in about a second and never measures anything, which is what makes
    # it admissible here at all. Without it the shell half of the tooling is
    # the only part with no lane, and it is the part that rots.
    echo "Running the performance campaign orchestrator self-test"
    scripts/test/test_perf_campaign.sh

    echo "Running matcher, documentation, and prepared-storage structural checks"
    {
        scripts/test_matcher_correctness.sh
        scripts/check-live-launchers.py
        cargo test --release -p lingrep-corpus --bench prepared_tokens
    } 2>&1 | tee /tmp/cargo-test-matcher-gate.log
}

# Both lanes, because a public item's doc linking a private one fails `cargo
# doc` while passing `cargo test` and `cargo clippy` — so without this the
# error reaches a contributor only when someone runs the full lane, or never.
#
# The second invocation is not redundant. `cargo doc --no-deps` selects
# targets, not features, so every `#[cfg(feature = "screen")]` module is
# invisible to it, including `lingrep_perf::orchestrate`. This mirrors the
# feature-gated clippy lane above.
# Both lanes, because the counted-work pin is what a matcher change moves and a
# matcher change is exactly what gets pushed after `--fast`. The clippy half
# stays in the full lane; this gate protects the pinned counters.
run_screen_lane() {
    echo "Running lingrep-perf with the counted-work screen enabled"
    cargo test -p lingrep-perf --features screen --quiet 2>&1 | tee /tmp/cargo-test-lingrep-perf.log

    # `lingrep-core`'s own instrumentation tests, which nothing else runs. The line
    # above tests `lingrep-perf`'s targets; it builds `lingrep-core` as a library, so
    # every `#[cfg(feature = "bench-instrumentation")]` test module inside
    # `lingrep-core` is neither compiled nor run — the same target-versus-feature
    # gap as the doc and clippy lanes below, one level further in.
    #
    # Without this lane, a change that moves a matcher pin under these features
    # passes every other lane. The feature set matches the line above,
    # so this reuses that build rather than compiling `lingrep-core` twice.
    cargo test -p lingrep-core --features testing,bench-instrumentation --lib --quiet 2>&1 |
        tee /tmp/cargo-test-lingrep-core-instrumentation.log
}

run_doc_lane() {
    echo "Building documentation with warnings denied"
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps 2>&1 | tee /tmp/cargo-doc.log

    echo "Building feature-gated documentation with warnings denied"
    RUSTDOCFLAGS="-D warnings" cargo doc -p lingrep-perf --features screen --no-deps 2>&1 |
        tee /tmp/cargo-doc-lingrep-perf.log
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
