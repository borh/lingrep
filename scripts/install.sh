#!/bin/sh
# scripts/install.sh — install `ling` from a lingrep release.
#
#   curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSLO https://github.com/borh/lingrep/releases/latest/download/install.sh
#   sh install.sh
#
# Each release publishes this file with its own version filled in (see
# scripts/render-installer.sh), so the copy from a release installs that
# release. The script downloads the archive for this computer, checks it
# against the release's SHA-256 checksum, and installs `ling` with its manual
# pages and shell completions in the XDG Base Directory locations of the
# current user. Requires no administrator rights. When the binary's directory
# is not on PATH, it adds one marked line to the shell's startup file.
#
# Downloads are HTTPS-only, including redirects. The checksum comes from the
# same release as the archive, so it detects a corrupted or truncated download,
# not a tampered release.
#
# Environment:
#   LING_VERSION         release to install, such as 0.3.2 (default: this script's release)
#   LING_INSTALL_DIR     directory for `ling` (default: $XDG_BIN_HOME, else ~/.local/bin)
#   LING_NO_MODIFY_PATH  1 leaves shell startup files unchanged
#   XDG_DATA_HOME        manual pages and completions go under it (default: ~/.local/share)
#   XDG_CONFIG_HOME      fish's PATH file goes under it (default: ~/.config)
#
# As the XDG Base Directory Specification requires, an XDG variable holding a
# relative path is ignored.
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

xdg_dir() { # value default
    case "$1" in
        /*) echo "$1" ;;
        *) echo "$2" ;;
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
        fish) echo "$(xdg_dir "${XDG_CONFIG_HOME:-}" "$HOME/.config")/fish/conf.d/lingrep.fish" ;;
        *) echo "$HOME/.profile" ;;
    esac
}

add_to_path() { # dir
    case ":${PATH:-}:" in
        *":$1:"*) return 0 ;;
    esac
    case "$1" in
        "$HOME"/*)
            # The startup file keeps $HOME unexpanded, so the line survives a renamed home.
            # shellcheck disable=SC2016
            shown='$HOME'"${1#"$HOME"}"
            ;;
        *) shown=$1 ;;
    esac
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

install_file() { # source destination mode
    staged="$(dirname "$2")/.$(basename "$2").install.$$"
    mkdir -p "$(dirname "$2")"
    cp "$1" "$staged"
    chmod "$3" "$staged"
    mv -f "$staged" "$2"
}

# Copies the manual pages and completions an archive carries; older archives
# without them install the binary alone.
install_data() { # unpacked archive, data directory
    for page in "$1"/share/man/man1/*.1; do
        [ -f "$page" ] && install_file "$page" "$2/man/man1/${page##*/}" 644
    done
    for file in bash-completion/completions/ling fish/vendor_completions.d/ling.fish zsh/site-functions/_ling; do
        [ -f "$1/share/$file" ] && install_file "$1/share/$file" "$2/$file" 644
    done
    return 0
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

    dir=${LING_INSTALL_DIR:-$(xdg_dir "${XDG_BIN_HOME:-}" "$HOME/.local/bin")}
    data=$(xdg_dir "${XDG_DATA_HOME:-}" "$HOME/.local/share")
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

    install_file "$tmp/$name/bin/ling" "$dir/ling" 755

    reported=$("$dir/ling" --version 2>&1) || die "$dir/ling does not run: $reported"
    [ "$reported" = "ling $version" ] || die "$dir/ling reports '$reported', expected 'ling $version'"
    say "installed $reported to $dir/ling"
    install_data "$tmp/$name" "$data"
    say "installed manual pages and shell completions under $data"

    add_to_path "$dir"
    found=$(command -v ling 2>/dev/null || true)
    if [ -n "$found" ] && [ "$found" != "$dir/ling" ]; then
        say "note: another ling at $found comes first on PATH"
    fi
    say "to analyze Japanese text, install a dictionary: ling dictionary install"
}

main "$@"
