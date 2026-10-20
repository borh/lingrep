#!/bin/sh
# scripts/install.sh — install `ling` from a lingrep release.
#
#   curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSLO https://github.com/borh/lingrep/releases/latest/download/install.sh
#   sh install.sh
#
# Each release publishes this file with its own version filled in (see
# scripts/render-installer.sh), so the copy from a release installs that
# release. The script downloads the archive for this computer, checks it
# against the release's SHA-256 checksum, and copies `ling` into
# ~/.local/bin. Requires no administrator rights. When that directory is not
# on PATH, it adds one marked line to the shell's startup file.
#
# Downloads are HTTPS-only, including redirects. The checksum comes from the
# same release as the archive, so it detects a corrupted or truncated download,
# not a tampered release.
#
# Environment:
#   LING_VERSION         release to install, such as 0.3.2 (default: this script's release)
#   LING_INSTALL_DIR     directory for `ling` (default: ~/.local/bin)
#   LING_NO_MODIFY_PATH  1 leaves shell startup files unchanged
#
# The body runs from main on the last line, so a download cut short runs
# nothing.

set -eu

RELEASE_VERSION='@LING_VERSION@'
REPOSITORY=https://github.com/borh/lingrep
MARKER='# Added by the lingrep installer'

say() {
    printf 'lingrep installer: %s\n' "$*"
}

die() {
    printf 'lingrep installer: error: %s\n' "$*" >&2
    exit 1
}

fetch() { # url output
    curl --proto '=https' --proto-redir '=https' --tlsv1.2 --fail --silent --show-error --location \
        --retry 3 --output "$2" "$1" || die "could not download $1"
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        die "neither sha256sum nor shasum is available to check the download"
    fi
}

check_glibc() {
    if ldd --version 2>&1 | grep -qi musl; then
        die "this Linux uses musl; release builds require glibc 2.35 or newer"
    fi
    glibc=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}') || glibc=
    [ -n "$glibc" ] || return 0
    major=${glibc%%.*}
    minor=${glibc#*.}
    minor=${minor%%.*}
    if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 35 ]; }; then
        die "glibc $glibc is too old; release builds require glibc 2.35 or newer (Ubuntu 22.04 or newer)"
    fi
}

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Darwin)
            # A shell running under Rosetta reports x86_64 on Apple silicon.
            if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
                arch=arm64
            fi
            case "$arch" in
                arm64 | aarch64) echo aarch64-apple-darwin ;;
                x86_64) echo x86_64-apple-darwin ;;
                *) die "no release build for macOS on $arch" ;;
            esac
            ;;
        Linux)
            check_glibc
            case "$arch" in
                x86_64 | amd64) echo x86_64-unknown-linux-gnu ;;
                aarch64 | arm64) echo aarch64-unknown-linux-gnu ;;
                *) die "no release build for Linux on $arch" ;;
            esac
            ;;
        MINGW* | MSYS* | CYGWIN*)
            die "there is no Windows build; run this installer in Ubuntu on WSL"
            ;;
        *) die "no release build for $os" ;;
    esac
}

startup_file() {
    case "$(basename "${SHELL:-sh}")" in
        zsh) echo "${ZDOTDIR:-$HOME}/.zshrc" ;;
        bash)
            if [ "$(uname -s)" = Darwin ]; then
                echo "$HOME/.bash_profile"
            else
                echo "$HOME/.bashrc"
            fi
            ;;
        fish) echo "$HOME/.config/fish/conf.d/lingrep.fish" ;;
        *) echo "$HOME/.profile" ;;
    esac
}

add_to_path() { # dir
    case ":${PATH:-}:" in
        *":$1:"*) return 0 ;;
    esac
    if [ "$1" = "$HOME/.local/bin" ]; then
        # The startup file keeps $HOME unexpanded, so the line survives a renamed home.
        # shellcheck disable=SC2016
        shown='$HOME/.local/bin'
    else
        shown=$1
    fi
    if [ "${LING_NO_MODIFY_PATH:-}" = 1 ]; then
        say "$1 is not on PATH; add it yourself, for example:"
        say "  export PATH=\"$shown:\$PATH\""
        return 0
    fi
    file=$(startup_file)
    case "$file" in
        *.fish) line="fish_add_path -g \"$shown\"" ;;
        *) line="export PATH=\"$shown:\$PATH\"" ;;
    esac
    if [ -f "$file" ] && grep -qF "$MARKER" "$file"; then
        say "$file already has the lingrep PATH line"
    else
        mkdir -p "$(dirname "$file")"
        printf '\n%s\n%s\n' "$MARKER" "$line" >>"$file"
        say "added $1 to PATH in $file"
    fi
    say "open a new terminal, or run this line in the current one:"
    say "  export PATH=\"$1:\$PATH\""
}

main() {
    [ -n "${HOME:-}" ] || die "HOME is not set"
    command -v curl >/dev/null 2>&1 || die "curl is required"
    command -v tar >/dev/null 2>&1 || die "tar is required"

    version=${LING_VERSION:-$RELEASE_VERSION}
    case "$version" in
        *@*) die "this copy was not published with a release; set LING_VERSION, such as LING_VERSION=0.3.2" ;;
    esac
    printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' \
        || die "LING_VERSION must look like 0.3.2, not '$version'"

    dir=${LING_INSTALL_DIR:-$HOME/.local/bin}
    case "$dir" in
        /*) ;;
        *) die "LING_INSTALL_DIR must be an absolute path" ;;
    esac
    # These characters would break the quoted PATH line written to the startup file.
    # shellcheck disable=SC1003
    case "$dir" in
        *'"'* | *'$'* | *'`'* | *'\'*) die "LING_INSTALL_DIR must not contain quotes, \$, \` or \\" ;;
    esac

    target=$(detect_target)
    name="ling-$version-$target"
    base="$REPOSITORY/releases/download/v$version"

    tmp=$(mktemp -d "${TMPDIR:-/tmp}/lingrep-install.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    trap 'exit 130' INT TERM

    say "downloading ling $version for $target"
    fetch "$base/$name.tar.gz" "$tmp/$name.tar.gz"
    fetch "$base/$name.tar.gz.sha256" "$tmp/$name.tar.gz.sha256"

    expected=$(awk 'NR == 1 {print $1}' "$tmp/$name.tar.gz.sha256")
    printf '%s\n' "$expected" | grep -Eq '^[0-9a-f]{64}$' || die "the release checksum file is malformed"
    actual=$(sha256_of "$tmp/$name.tar.gz")
    [ "$actual" = "$expected" ] || die "checksum mismatch for $name.tar.gz (expected $expected, got $actual)"

    if tar -tzf "$tmp/$name.tar.gz" | grep -v "^$name/" | grep -q .; then
        die "$name.tar.gz has entries outside $name/"
    fi
    tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
    [ -f "$tmp/$name/bin/ling" ] || die "$name.tar.gz has no bin/ling"

    mkdir -p "$dir"
    cp "$tmp/$name/bin/ling" "$dir/.ling.install.$$"
    chmod 755 "$dir/.ling.install.$$"
    mv -f "$dir/.ling.install.$$" "$dir/ling"

    reported=$("$dir/ling" --version 2>&1) || die "$dir/ling does not run: $reported"
    [ "$reported" = "ling $version" ] || die "$dir/ling reports '$reported', expected 'ling $version'"
    say "installed $reported to $dir/ling"

    add_to_path "$dir"
    found=$(command -v ling 2>/dev/null || true)
    if [ -n "$found" ] && [ "$found" != "$dir/ling" ]; then
        say "note: another ling at $found comes first on PATH"
    fi
}

main "$@"
