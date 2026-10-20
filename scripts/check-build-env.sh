#!/usr/bin/env bash
# scripts/check-build-env.sh
# Fail loudly when cargo would run outside the Nix dev shell.
#
# The dev shell supplies dependencies and build flags that affect Cargo
# fingerprints; reject a mismatched environment before compiling.
set -euo pipefail

fail() {
    echo "error: $1" >&2
    echo "       Run inside the dev shell:  nix develop -c <command>" >&2
    exit 1
}

[ -n "${LINGREP_DICTIONARY_PATH:-}" ] || fail "LINGREP_DICTIONARY_PATH is unset"
[ "${CARGO_INCREMENTAL:-}" = "1" ] || fail "CARGO_INCREMENTAL is '${CARGO_INCREMENTAL:-unset}', expected 1"

# The linker is the one dev-shell setting that differs by platform. mold has no
# Mach-O backend, so darwin takes the default ld64 and RUSTFLAGS is empty
# there; asserting mold everywhere would fail every macOS shell. Keep this in
# step with the `RUSTFLAGS` definition in flake.nix.
case "$(uname -s)" in
    Darwin)
        case "${RUSTFLAGS:-}" in
            *fuse-ld=*) fail "RUSTFLAGS selects a linker on darwin (got '$RUSTFLAGS'); expected the default ld64" ;;
        esac
        ;;
    *)
        case "${RUSTFLAGS:-}" in
            *fuse-ld=mold*) ;;
            *) fail "RUSTFLAGS does not select mold (got '${RUSTFLAGS:-unset}')" ;;
        esac
        command -v mold >/dev/null || fail "mold is not on PATH"
        ;;
esac

echo "build environment OK"
