# Changelog

All notable changes to lingrep are recorded in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.2] - 2026-10-20

### Added

- A `ling` archive for Intel Macs (`x86_64-apple-darwin`).
- `install.sh`, published with each release: it installs that release's
  `ling` for the computer into `~/.local/bin` after checking its SHA-256
  checksum, and puts the directory on `PATH`. See the
  [release notes](docs/release-notes/v0.3.2.md).

## [0.3.1] - 2026-10-20

### Fixed

- The macOS archive no longer requires Homebrew `xz`: liblzma is linked
  statically. See the [release notes](docs/release-notes/v0.3.1.md).

## [0.3.0] - 2026-10-20

First public release. See the [release notes](docs/release-notes/v0.3.0.md).
