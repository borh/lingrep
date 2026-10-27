#!/usr/bin/env bash

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
if [[ ! -x "$repo_root/ci.sh" ]]; then
    echo "ci.sh must remain directly executable" >&2
    exit 1
fi
temporary=$(mktemp -d)
cleanup() {
    local status=$?
    if [[ $status -ne 0 ]]; then
        cat "$temporary/stdout.log" "$temporary/stderr.log" >&2
    fi
    rm -rf "$temporary"
    exit "$status"
}
trap cleanup EXIT

mkdir -p "$temporary/bin" "$temporary/scripts/test" "$temporary/inputs"
cp "$repo_root/ci.sh" "$temporary/ci.sh"
chmod +x "$temporary/ci.sh"

make_script_stub() {
    local name=$1
    cat >"$temporary/scripts/$name" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
name=${0##*/}
printf 'script:%s\n' "$name" >>"$LINGREP_CI_TEST_LOG"
if [[ ${LINGREP_CI_FAIL_STEP:-} == "$name" ]]; then
    exit 42
fi
EOF
    chmod +x "$temporary/scripts/$name"
}

for script in "$repo_root"/scripts/*.sh "$repo_root"/scripts/*.py "$repo_root"/scripts/test/*.sh; do
    make_script_stub "${script#"$repo_root/scripts/"}"
done

for command_name in uv python; do
    cat >"$temporary/bin/$command_name" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s:%s\n' "${0##*/}" "$*" >>"$LINGREP_CI_TEST_LOG"
if [[ ${LINGREP_CI_FAIL_STEP:-} == "${0##*/}:$*" ]]; then
    exit 42
fi
EOF
    chmod +x "$temporary/bin/$command_name"
done

cat >"$temporary/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'cargo:%s\n' "$*" >>"$LINGREP_CI_TEST_LOG"
if [[ ${LINGREP_CI_FAIL_STEP:-} == cargo || ${LINGREP_CI_FAIL_STEP:-} == "cargo:$*" ]]; then
    exit 42
fi
if [[ ${1:-} == metadata ]]; then
    printf '%s\n' '{"packages":[]}'
fi
EOF

cat >"$temporary/bin/jq" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'jq:%s\n' "$*" >>"$LINGREP_CI_TEST_LOG"
cat >/dev/null
exit 1
EOF

cat >"$temporary/bin/node" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'node:%s\n' "$*" >>"$LINGREP_CI_TEST_LOG"
EOF

for command_name in chromium chromedriver cargo-fuzz; do
    cat >"$temporary/bin/$command_name" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
    chmod +x "$temporary/bin/$command_name"
done
chmod +x "$temporary/bin/cargo" "$temporary/bin/jq" "$temporary/bin/node"

touch "$temporary/inputs/corpus.jsonl" "$temporary/inputs/patterns.lx"

export PATH="$temporary/bin:$PATH"
export LINGREP_CI_TEST_LOG="$temporary/invocations.log"
export LINGREP_DICTIONARY_PATH="$temporary/inputs/dictionary.dic.zst"
export LINGREP_REAL_CORPUS_PATH="$temporary/inputs/corpus.jsonl"
export LINGREP_REAL_PATTERNS_PATH="$temporary/inputs/patterns.lx"

run_mode() {
    : >"$LINGREP_CI_TEST_LOG"
    (cd "$temporary" && "$temporary/ci.sh" "$@") \
        >"$temporary/stdout.log" 2>"$temporary/stderr.log"
}

assert_has() {
    grep -Fx -- "$1" "$LINGREP_CI_TEST_LOG" >/dev/null
}

assert_lacks_prefix() {
    if grep -F -- "$1" "$LINGREP_CI_TEST_LOG" >/dev/null; then
        echo "unexpected invocation containing: $1" >&2
        cat "$LINGREP_CI_TEST_LOG" >&2
        exit 1
    fi
}

assert_full_contract() {
    assert_has "python:scripts/check-adapter-wheels.py"
    assert_has "cargo:test --workspace --tests --quiet"
    assert_has "script:cargo-test-selected.py"
    [[ $(grep -Fxc "cargo:test --workspace --tests --quiet" "$LINGREP_CI_TEST_LOG") -eq 1 ]]
    assert_has "script:run_lingrep_web_layout_e2e.sh"
    assert_has "script:test_matcher_correctness.sh"
    assert_has "script:check-live-launchers.py"
    assert_has "cargo:test --release -p lingrep-corpus --bench prepared_tokens"
    assert_has "cargo:doc --no-deps"
    assert_lacks_prefix "script:run_fuzz_smoke.sh"
    assert_lacks_prefix "cargo:bench "
}

run_mode
assert_full_contract

run_mode --full
assert_full_contract

run_mode --quick
assert_full_contract
grep -F "deprecated" "$temporary/stderr.log" >/dev/null

run_mode --fast
assert_has "python:scripts/check-adapter-wheels.py"
assert_has "script:check-build-env.sh"
assert_has "script:test_ci_modes.sh"
assert_has "script:test_fuzz_smoke.sh"
assert_has "cargo:fmt --check --all"
assert_has "cargo:clippy --all-targets -- -D warnings"
assert_has "cargo:test --workspace --lib --bins --quiet"
assert_has "node:--test lingrep-web/js-tests/*.test.mjs"
assert_has "cargo:test --workspace --doc"
assert_lacks_prefix "cargo:test --workspace --tests"
assert_lacks_prefix "script:run_lingrep_web_layout_e2e.sh"
assert_lacks_prefix "script:run_fuzz_smoke.sh"
assert_lacks_prefix "cargo:bench "

run_mode --fuzz
assert_has "script:check-build-env.sh"
assert_has "script:run_fuzz_smoke.sh"
[[ $(wc -l <"$LINGREP_CI_TEST_LOG") -eq 2 ]]

run_mode --performance
assert_has "script:check-build-env.sh"
assert_has "cargo:bench -p lingrep-corpus --bench prepared_tokens"
assert_has "cargo:bench -p lingrep-core --bench fsm_performance"
assert_has "cargo:bench -p lingrep-core --bench corpus_discovery"
assert_has "script:measure_sentence_final_profile.sh"
[[ $(wc -l <"$LINGREP_CI_TEST_LOG") -eq 5 ]]

if (cd "$temporary" && "$temporary/ci.sh" --unknown) >/dev/null 2>&1; then
    echo "unknown mode must fail" >&2
    exit 1
fi
if (cd "$temporary" && "$temporary/ci.sh" --fast extra) >/dev/null 2>&1; then
    echo "extra arguments must fail" >&2
    exit 1
fi

for failure in \
    'python:scripts/check-adapter-wheels.py' \
    check-test-consolidation.sh \
    cargo-test-selected.py \
    'cargo:fmt --check --all' \
    'cargo:test --workspace --tests --quiet' \
    test_matcher_correctness.sh
do
    export LINGREP_CI_FAIL_STEP=$failure
    if run_mode --full; then
        echo "selected-step failure must propagate: $failure" >&2
        exit 1
    else
        status=$?
        [[ $status -eq 42 ]]
    fi
    if grep -F "CI lane passed" "$temporary/stdout.log" >/dev/null; then
        echo "failed lane must not report success: $failure" >&2
        exit 1
    fi
    assert_lacks_prefix "cargo:doc "
done
unset LINGREP_CI_FAIL_STEP

rm "$temporary/bin/chromedriver"
for command_name in bash cat dirname tee; do
    ln -s "$(type -P "$command_name")" "$temporary/bin/$command_name"
done
original_path=$PATH
export PATH="$temporary/bin"
if run_mode --full; then
    echo "full mode must fail when browser prerequisites are absent" >&2
    exit 1
fi
export PATH=$original_path
grep -F "full CI requires chromium and chromedriver" "$temporary/stderr.log" >/dev/null
