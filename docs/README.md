# lingrep documentation

Start with the repository [README](../README.md) for installation and a first
query. Installed `ling --help`, subcommand help, and manual pages are the
authority for the command line; `ling check --help` is the complete lingrep patterns
authoring reference.

## Use ling

| Guide | Purpose |
| --- | --- |
| [CLI usage](cli-usage-guide.md) | Matching, aggregation, corpus preparation, formats, and machine-output contracts |
| [TUI](tui-guide.md) | Interactive terminal exploration and pattern editing |
| [Markdown corpus dialect](markdown-corpus-dialect.md) | Markdown admission, section labels, and selection |
| [TSV contracts](lingrep-tsv-contracts.md) | Generated current TSV field and ordering contracts |
| [N-gram analysis](ngram-analysis.md) | One-to-five-token windows expressed with lingrep patterns |
| [Gravity analysis](gravity-analysis.md) | External corpus-global analysis over query JSONL |
| [Progress reporting](progress-reporting-guide.md) | Human and machine progress behavior |

Release notes for published versions live in [release-notes/](release-notes/).

## Deploy lingrep-web

The [deployment guide](deployment-guide.md) covers the packaged web
application, NixOS module, reverse proxy configuration, corpus ownership, and
multiple-corpus setup.

## Develop the project

| Guide | Purpose |
| --- | --- |
| [Development environment](development-environment.md) | Supported Nix shells, builds, and validation lanes |
| [Contributing](../CONTRIBUTING.md) | Change workflow and verification expectations |
| [Testing](testing-guide.md) | Test inventory, focused commands, and test utilities |
| [Matcher development](fsm-development.md) | Current compiler, matcher, and scheduling architecture |
| [lingrep patterns reference](pattern-language-reference.md) | Every construct, attribute mode, metadata key, and policy statement |
| [lingrep patterns co-development](pattern-language-co-development.md) | Coordinated grammar, compiler, CLI-reference, and browser changes |
| [Parser limitations](parser-limitations.md) | Strict source acceptance and unsupported constructs |
| [Performance requirements](PERFORMANCE_REQUIREMENTS.md) | Targets, evidence tiers, and acceptance workflow |
| [Performance guide](PERFORMANCE_GUIDE.md) | Applying the performance workflow to supported paths |
| [Lint policy](lint-policy.md) | Workspace Clippy policy and its exceptions |

Dictionary packages and their intended text domains are documented in
[DICTIONARIES.md](../DICTIONARIES.md).

## Agent instructions

[AGENTS.md](../AGENTS.md) is the single persistent agent guide; `CLAUDE.md` is
a symlink to it. It routes agents to the guides above and records
architecture rules and prohibited APIs.

## Adding documentation

Put current user or developer guidance in `docs/` and add it to the appropriate
table above. Keep generated logs, campaign output, measurements, and planning
records outside the source tree.
