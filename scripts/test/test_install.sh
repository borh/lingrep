#!/usr/bin/env bash
# Exercises the release installer against a local fake release. Stub `curl`,
# `uname`, `sysctl`, `getconf` and `ldd` stand in for the network and the
# platform, so the test requires neither a network nor macOS. Runs the rendered
# installer under every POSIX shell found (sh, dash).

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT

version=9.8.7
installer="$temporary/install.sh"
"$repo_root/scripts/render-installer.sh" "$version" "$installer"

release="$temporary/release"
stubs="$temporary/stubs"
mkdir -p "$release" "$stubs"

sha256() {
    if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi
}

make_archive() { # target [extra member]
    local name="ling-$version-$1" work="$temporary/archive-$1"
    rm -rf "$work"
    mkdir -p "$work/$name/bin"
    printf '#!/bin/sh\necho "ling %s"\n' "$version" >"$work/$name/bin/ling"
    chmod 755 "$work/$name/bin/ling"
    mkdir -p "$work/$name/share/man/man1" "$work/$name/share/bash-completion/completions" \
        "$work/$name/share/fish/vendor_completions.d" "$work/$name/share/zsh/site-functions"
    printf '.TH LING 1\n' >"$work/$name/share/man/man1/ling.1"
    printf '.TH LING-MATCHES 1\n' >"$work/$name/share/man/man1/ling-matches.1"
    printf '# bash\n' >"$work/$name/share/bash-completion/completions/ling"
    printf '# fish\n' >"$work/$name/share/fish/vendor_completions.d/ling.fish"
    printf '#compdef ling\n' >"$work/$name/share/zsh/site-functions/_ling"
    if [[ $# -gt 1 ]]; then
        printf 'x\n' >"$work/$2"
        tar -C "$work" -czf "$release/$name.tar.gz" "$name" "$2"
    else
        tar -C "$work" -czf "$release/$name.tar.gz" "$name"
    fi
    (cd "$release" && sha256 "$name.tar.gz" >"$name.tar.gz.sha256")
}

for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu aarch64-apple-darwin x86_64-apple-darwin; do
    make_archive "$target"
done

# curl: refuse anything but the HTTPS-only flags, then serve the release
# directory by file name and log the URL.
cat >"$stubs/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ " $* " == *" --proto =https "* && " $* " == *" --proto-redir =https "* && " $* " == *" --tlsv1.2 "* ]] || { echo "curl stub: missing HTTPS-only flags" >&2; exit 90; }
output= url=
while [[ $# -gt 0 ]]; do
    case $1 in
        --output) output=$2; shift 2 ;;
        --proto | --proto-redir | --retry) shift 2 ;;
        -*) shift ;;
        *) url=$1; shift ;;
    esac
done
printf '%s\n' "$url" >>"$TEST_CURL_LOG"
[[ $url == https://github.com/borh/lingrep/releases/download/v* ]] || exit 22
cp "$TEST_RELEASE/${url##*/}" "$output" 2>/dev/null || exit 22
EOF
cat >"$stubs/uname" <<'EOF'
#!/bin/sh
case $1 in -s) echo "$TEST_UNAME_S" ;; -m) echo "$TEST_UNAME_M" ;; esac
EOF
cat >"$stubs/sysctl" <<'EOF'
#!/bin/sh
echo "${TEST_TRANSLATED:-0}"
EOF
cat >"$stubs/getconf" <<'EOF'
#!/bin/sh
echo "glibc ${TEST_GLIBC:-2.39}"
EOF
cat >"$stubs/ldd" <<'EOF'
#!/bin/sh
echo "${TEST_LDD:-ldd (GNU libc) 2.39}"
EOF
chmod 755 "$stubs"/*

shells=(sh)
command -v dash >/dev/null && shells+=(dash)

failures=0
fail() {
    echo "FAIL [$shell] $*" >&2
    failures=$((failures + 1))
}

# run <expected exit: 0 or 1> [VAR=value ...] — runs the installer in a fresh
# HOME under $shell and leaves its output in $out.
run() {
    local expect=$1 status=0
    shift
    home="$temporary/home"
    rm -rf "$home" "$temporary/curl.log"
    mkdir -p "$home"
    out=$(env -i HOME="$home" SHELL=/bin/bash TMPDIR="$temporary" \
        PATH="$stubs:$PATH" TEST_RELEASE="$release" TEST_CURL_LOG="$temporary/curl.log" \
        TEST_UNAME_S=Linux TEST_UNAME_M=x86_64 "$@" "$shell" "$installer" 2>&1) || status=$?
    if [[ $expect == 0 && $status != 0 ]] || [[ $expect == 1 && $status == 0 ]]; then
        fail "expected exit $expect, got $status for $*: $out"
        return 1
    fi
}

requested() { grep -c "/$1.tar.gz\$" "$temporary/curl.log" 2>/dev/null || true; }

for shell in "${shells[@]}"; do
    # Linux x86-64: installs, adds one PATH line, and a second run adds no more.
    if run 0; then
        [[ $("$home/.local/bin/ling" --version) == "ling $version" ]] || fail "installed binary"
        [[ $(grep -c 'Added by the lingrep installer' "$home/.bashrc") == 1 ]] || fail "PATH line"
        grep -qx 'export PATH="$HOME/.local/bin:$PATH"' "$home/.bashrc" || fail "PATH line text"
        printf '#!/bin/sh\necho "ling 0.0.1"\n' >"$home/.local/bin/ling"
        status=0
        env -i HOME="$home" SHELL=/bin/bash TMPDIR="$temporary" PATH="$stubs:$PATH" \
            TEST_RELEASE="$release" TEST_CURL_LOG="$temporary/curl.log" \
            TEST_UNAME_S=Linux TEST_UNAME_M=x86_64 "$shell" "$installer" >/dev/null 2>&1 || status=$?
        [[ $status == 0 && $(grep -c 'Added by the lingrep installer' "$home/.bashrc") == 1 ]] || fail "rerun duplicated the PATH line"
        [[ $("$home/.local/bin/ling" --version) == "ling $version" ]] || fail "upgrade did not replace the previous binary"
    fi

    # Manual pages and completions go under the default XDG data directory.
    if run 0; then
        for file in man/man1/ling.1 man/man1/ling-matches.1 bash-completion/completions/ling \
            fish/vendor_completions.d/ling.fish zsh/site-functions/_ling; do
            [[ -f "$home/.local/share/$file" ]] || fail "default data file $file"
        done
        [[ -z $(find "$home" -name '*.install.*') ]] || fail "staged file left behind"
    fi

    # XDG_BIN_HOME and XDG_DATA_HOME relocate the binary and its data; the PATH
    # line keeps $HOME unexpanded for a directory inside it.
    if run 0 XDG_BIN_HOME="$temporary/home/bin" XDG_DATA_HOME="$temporary/home/data"; then
        [[ -x "$home/bin/ling" && ! -e "$home/.local/bin/ling" ]] || fail "XDG_BIN_HOME"
        [[ -f "$home/data/man/man1/ling.1" && ! -e "$home/.local/share" ]] || fail "XDG_DATA_HOME"
        grep -qx 'export PATH="$HOME/bin:$PATH"' "$home/.bashrc" || fail "PATH line under HOME"
    fi
    run 0 LING_INSTALL_DIR="$temporary/home/explicit" XDG_BIN_HOME="$temporary/home/bin" \
        && [[ -x "$home/explicit/ling" && ! -e "$home/bin" ]] || fail "LING_INSTALL_DIR overrides XDG_BIN_HOME"

    # A relative XDG path is invalid under the specification and is ignored.
    run 0 XDG_BIN_HOME=bin XDG_DATA_HOME=data \
        && [[ -x "$home/.local/bin/ling" && -f "$home/.local/share/man/man1/ling.1" ]] || fail "relative XDG paths"

    # fish reads its PATH file from XDG_CONFIG_HOME.
    run 0 SHELL=/usr/bin/fish XDG_CONFIG_HOME="$temporary/home/config" \
        && grep -qx 'fish_add_path -g "$HOME/.local/bin"' "$home/config/fish/conf.d/lingrep.fish" || fail "fish XDG_CONFIG_HOME"

    # Platform detection picks the archive for each supported computer.
    run 0 TEST_UNAME_M=aarch64 && [[ $(requested "ling-$version-aarch64-unknown-linux-gnu") == 1 ]] || fail "Linux arm64"
    run 0 TEST_UNAME_S=Darwin TEST_UNAME_M=arm64 && [[ $(requested "ling-$version-aarch64-apple-darwin") == 1 ]] || fail "macOS arm64"
    run 0 TEST_UNAME_S=Darwin TEST_UNAME_M=x86_64 && [[ $(requested "ling-$version-x86_64-apple-darwin") == 1 ]] || fail "macOS Intel"
    run 0 TEST_UNAME_S=Darwin TEST_UNAME_M=x86_64 TEST_TRANSLATED=1 && [[ $(requested "ling-$version-aarch64-apple-darwin") == 1 ]] || fail "Rosetta shell on Apple silicon"
    run 0 TEST_UNAME_S=Darwin TEST_UNAME_M=arm64 SHELL=/bin/zsh && [[ -f "$home/.zshrc" ]] || fail "zsh startup file"

    # Refusals install nothing.
    run 1 TEST_UNAME_S=MINGW64_NT-10.0 && [[ $out == *WSL* ]] || fail "Windows refusal names WSL"
    run 1 TEST_GLIBC=2.31 && [[ $out == *"glibc 2.31 is too old"* ]] || fail "old glibc refusal"
    run 1 TEST_LDD='musl libc (x86_64)' && [[ $out == *musl* ]] || fail "musl refusal"
    run 1 LING_VERSION=latest && [[ $out == *"must look like"* ]] || fail "version validation"
    run 1 LING_INSTALL_DIR=relative/bin && [[ $out == *absolute* ]] || fail "relative install dir"
    [[ ! -e "$home/.local/bin/ling" ]] || fail "a refusal left a binary behind"

    # LING_NO_MODIFY_PATH leaves startup files alone; a directory on PATH requires no line.
    run 0 LING_NO_MODIFY_PATH=1 && [[ ! -e "$home/.bashrc" && $out == *"not on PATH"* ]] || fail "LING_NO_MODIFY_PATH"
    run 0 LING_INSTALL_DIR="$temporary/onpath" PATH="$temporary/onpath:$stubs:$PATH" && [[ ! -e "$home/.bashrc" ]] || fail "directory already on PATH"

    # A tampered archive fails the checksum and installs nothing.
    cp "$release/ling-$version-x86_64-unknown-linux-gnu.tar.gz" "$temporary/good.tar.gz"
    printf 'tampered' >>"$release/ling-$version-x86_64-unknown-linux-gnu.tar.gz"
    run 1 && [[ $out == *"checksum mismatch"* && ! -e "$home/.local/bin/ling" ]] || fail "checksum mismatch"
    cp "$temporary/good.tar.gz" "$release/ling-$version-x86_64-unknown-linux-gnu.tar.gz"

    # An archive with an entry outside its top-level directory is refused.
    make_archive x86_64-unknown-linux-gnu stray-file
    run 1 && [[ $out == *"entries outside"* && ! -e "$home/.local/bin/ling" ]] || fail "stray archive entry"
    make_archive x86_64-unknown-linux-gnu

    # A copy that is not from a release asks for LING_VERSION.
    status=0
    out=$(env -i HOME="$temporary/home" PATH="$stubs:$PATH" TEST_UNAME_S=Linux TEST_UNAME_M=x86_64 \
        "$shell" "$repo_root/scripts/install.sh" 2>&1) || status=$?
    [[ $status != 0 && $out == *"set LING_VERSION"* ]] || fail "unpublished copy"

    # A download cut short runs nothing: main is only called on the last line.
    rm -rf "$temporary/home" "$temporary/curl.log"
    mkdir -p "$temporary/home"
    head -c "$(($(wc -c <"$installer") - 20))" "$installer" | env -i HOME="$temporary/home" SHELL=/bin/bash \
        PATH="$stubs:$PATH" TEST_RELEASE="$release" TEST_CURL_LOG="$temporary/curl.log" \
        TEST_UNAME_S=Linux TEST_UNAME_M=x86_64 "$shell" >/dev/null 2>&1 || true
    [[ ! -e "$temporary/curl.log" && -z $(ls -A "$temporary/home") ]] || fail "truncated installer ran"
done

if [[ $failures != 0 ]]; then
    echo "$failures installer check(s) failed" >&2
    exit 1
fi
echo "installer checks passed under: ${shells[*]}"
