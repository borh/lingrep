set shell := ["bash", "-euo", "pipefail", "-c"]

# Preview Cargo artifacts older than two days.
clean-dry-run:
    cargo sweep --dry-run --time 2 .

# Reclaim Cargo artifacts older than two days while preserving recent builds.
clean:
    cargo sweep --time 2 .

# Run the source-tree ling binary inside the active Nix development shell.
[positional-arguments]
ling *args:
    cargo run -q -p lingrep -- "$@"

# Run the focused ling tests, executable-launch guard, and Nix app checks.
check-ling:
    cargo test -p lingrep --tests
    scripts/check-ling-test-launches.sh
    test "$(nix eval --raw .#apps.x86_64-linux.default.program)" = "$(nix eval --raw .#apps.x86_64-linux.lingrep.program)"
    test "$(basename "$(nix eval --raw .#apps.x86_64-linux.lingrep.program)")" = ling

# Screen counted matcher work for every tracked catalog (or one with --catalog).
[positional-arguments]
perf-work *args:
    cargo run -q -p lingrep-perf --features screen -- work "$@"

# Run a paired campaign. `new` declares it, `suite` measures every catalog to
# the coverage its tier requires, `verdict` adjudicates.
[positional-arguments]
perf-campaign *args:
    scripts/perf_campaign.sh "$@"

# What each catalog in a campaign still owes.
[positional-arguments]
perf-plan dir:
    scripts/perf_campaign.sh plan --dir "$1"

# Adjudicate a campaign file and render its results document.
[positional-arguments]
perf-adjudicate campaign:
    cargo run -q -p lingrep-perf -- adjudicate --campaign "$1"
