# Changelog

All notable changes to lingrep are recorded in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- `install.sh` follows the XDG Base Directory Specification: `ling` goes to
  `$XDG_BIN_HOME` when set, and the manual pages and Bash, fish and zsh
  completions from the archive are installed under `$XDG_DATA_HOME`
  (default `~/.local/share`). See [Install ling](docs/installation.md).
- `ling` and `ling -h` describe what each command produces and end with
  example invocations.

## [0.3.2] - 2026-10-20

### Added

- A `ling` archive for Intel Macs (`x86_64-apple-darwin`).
- `install.sh`, published with each release: installs that release's
  `ling` for the host platform into `~/.local/bin` after verifying its SHA-256
  checksum, and adds the directory to `PATH`. See the
  [release notes](docs/release-notes/v0.3.2.md).

## [0.3.1] - 2026-10-20

### Fixed

- The macOS archive no longer requires Homebrew `xz`: liblzma is linked
  statically. See the [release notes](docs/release-notes/v0.3.1.md).

## [0.3.0] - 2026-10-20

First public release. See the [release notes](docs/release-notes/v0.3.0.md).
