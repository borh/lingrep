# Install ling

`ling` is published for Linux (x86-64 and arm64, glibc 2.35 or newer) and
macOS (Apple silicon and Intel). On Windows, install it in WSL. Release
archives contain no dictionary; `ling dictionary install` downloads one (see
[DICTIONARIES.md](../DICTIONARIES.md)).

## Install script

```bash
curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSLO https://github.com/borh/lingrep/releases/latest/download/install.sh
sh install.sh
```

The script downloads the archive for the host platform from the release it was
published with, checks the archive against the release's SHA-256 checksum, and
installs these files for the current user:

| File | Location |
|---|---|
| `ling` | `$XDG_BIN_HOME`, or `~/.local/bin` when it is unset |
| Manual pages | `$XDG_DATA_HOME/man/man1`, or `~/.local/share/man/man1` |
| Bash completion | `$XDG_DATA_HOME/bash-completion/completions/ling` |
| fish completion | `$XDG_DATA_HOME/fish/vendor_completions.d/ling.fish` |
| zsh completion | `$XDG_DATA_HOME/zsh/site-functions/_ling` |

When the directory that holds `ling` is not on `PATH`, the script appends a
marked line to the shell's startup file: `~/.bashrc` (`~/.bash_profile` on
macOS), `~/.zshrc` (`$ZDOTDIR/.zshrc` when set), `$XDG_CONFIG_HOME/fish/conf.d/lingrep.fish`, or
`~/.profile` for other shells. Running the script again replaces the installed
files and does not add a second line. To update an installer-managed installation,
download the latest `install.sh` again with the commands above and run it using
the same install-directory settings. Set `LING_VERSION` to select a specific
release or roll back, then check `ling --version`. Check the release notes for
prepared-corpus format changes before updating; incompatible artifacts must be
rebuilt from source. Update Nix, Cargo, and distribution-managed installations
through their original package manager. The script requires no administrator
rights. [scripts/install.sh](../scripts/install.sh) is its source.

| Variable | Effect |
|---|---|
| `LING_VERSION=0.3.2` | Install that release instead of the script's own |
| `LING_INSTALL_DIR=/path/to/bin` | Install `ling` there; takes precedence over `XDG_BIN_HOME` |
| `LING_NO_MODIFY_PATH=1` | Leave shell startup files unchanged |
| `XDG_BIN_HOME`, `XDG_DATA_HOME`, `XDG_CONFIG_HOME` | Move the locations above; a relative path is ignored, as the [XDG Base Directory Specification](https://specifications.freedesktop.org/basedir-spec/latest/) requires |

Bash and fish load the completions from these locations without further
setup. zsh does not search `$XDG_DATA_HOME/zsh/site-functions`; to use the zsh
completion, add this line to `~/.zshrc` before `compinit` runs:

```zsh
fpath=("${XDG_DATA_HOME:-$HOME/.local/share}/zsh/site-functions" $fpath)
```

## Download an archive

Each [release](https://github.com/borh/lingrep/releases) has one archive per
platform. An archive unpacks to a single directory containing `bin/ling`, shell
completions, manual pages, the license, and third-party notices.

```bash
version=0.5.0
target=aarch64-apple-darwin  # or x86_64-apple-darwin, x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu
base=https://github.com/borh/lingrep/releases/download/v$version
curl -LO "$base/ling-$version-$target.tar.gz"
curl -LO "$base/ling-$version-$target.tar.gz.sha256"
shasum -a 256 -c "ling-$version-$target.tar.gz.sha256"
tar xzf "ling-$version-$target.tar.gz"
export PATH="$PWD/ling-$version-$target/bin:$PATH"
ling --version
```

## Nix

```bash
nix run github:borh/lingrep#ling -- --help
```
