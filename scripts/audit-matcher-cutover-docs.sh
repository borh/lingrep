#!/usr/bin/env bash
# Detect user-facing documentation that presents retired matcher controls as live.
set -euo pipefail

# Include generic descriptions of the retired engine/control stack. Exact
# spellings here deliberately cover the old baseline claims as well as API and
# flag names; an active document must frame every hit locally.
readonly retired='vp-pattern-matcher|vp_pattern_matcher|2026-07-12-supported-cache-baseline(\.md)?|FSMTrait|FSM::new|RawMatcher|find_matches_raw|find_matches_with_source|MatchingStrategy|set_(dfa|parallel)_enabled|find_matches_adaptive|all[_ -]?with[_ -]?simd|DFA only|Parallel only|DFA for deterministic matching|Parallel execution across threads|SIMD pre-filtering|TokenBatch|TokenArena|CompactToken|create_token_batch|find_matches_batch|match_pattern_simd|\.vp[14](\.zst)?|--(cache-format|cache-path|no-cache|skip-cache-creation|chunked-processing|chunk-size|enable-dfa|enable-parallel|enable-prefilter|disable-prefilter|enable-simd|enable-simd-prefilter|disable-simd-prefilter|use-raw-matcher|processing-mode|max-matches|timeout-ms)'
readonly framing='historical|retired|reject|unsupported|unavailable|non-default|experiment|re-entry|future executor|not (a )?current|must not|outside the production'
# These benchmark targets were deleted with the former CLI/cache executors.
# Current contributor guidance selects retained targets by concern: prepared_tokens
# for prepared-artifact work, fsm_performance for matcher work, and
# corpus_discovery for recursive-admission work.
readonly deleted_targets='tokens_v5|tokens_v6|optimization_comparison|cache_format_benchmark|cache_format_comparison|production_matcher_bench|comprehensive_validation_bench|fsm_benchmarks?|raw_matcher_concurrent|nfa_vs_dfa|parallel_speedup|parallel_4threads|ooc_characterization'

readonly -a roots=(
  AGENTS.md README.md CLAUDE.md CONTRIBUTING.md CHANGELOG.md DICTIONARIES.md
  lingrep/README.md lingrep-core/benches
  docs
)

status=0
while IFS=: read -r file line content; do
  # The enhanced dependency component is a current, independently versioned
  # prepared-corpus format. Do not mistake its exact name for the retired
  # matcher-cache `.vp1` suffix; any other retired term on the line remains.
  content_without_current_components=${content//enhanced.v1.zst/}
  if ! grep -Eiq "${retired}" <<<"$content_without_current_components"; then
    continue
  fi

  # Released changelog sections are immutable historical records. The live
  # `[Unreleased]` section remains audited line by line.
  if [[ "$file" == "CHANGELOG.md" ]]; then
    first_release=$(awk '/^## \[[0-9]/{ print NR; exit }' "$file")
    if [[ -n "$first_release" ]] && (( line >= first_release )); then
      continue
    fi
  fi

  # A whole document is exempt only when its first non-empty line uses the
  # canonical historical-record marker. Incidental uses of "historical" must
  # still frame each retired term locally. Archives, plans, and release notes
  # are identified by their historical marker.
  if awk '
    BEGIN { status = 1 }
    /^[[:space:]]*$/ { next }
    /^>[[:space:]]*\*\*Historical record — .+\*\*[[:space:]]*$/ {
      status = 0
      exit
    }
    { exit }
    END { exit status }
  ' "$file"; then
    continue
  fi

  # A framing marker must be in the same paragraph or the immediately preceding
  # prose block (for a retired-flags code block). A nearby, unrelated negative
  # statement must not waive a live-looking example.
  if ! awk -v target="$line" -v framing="$framing" '
    function blank(value) { return value ~ /^[[:space:]]*$/ }
    function in_fenced_code(target, i, fences) {
      fences = 0
      for (i = 1; i <= target; i++) {
        if (lines[i] ~ /^[[:space:]]*```/) fences++
      }
      return fences % 2 == 1
    }
    function has_framing(value) { return value ~ framing }
    { lines[NR] = tolower($0) }
    END {
      start = target
      while (start > 1 && !blank(lines[start - 1])) start--
      end = target
      while (end < NR && !blank(lines[end + 1])) end++
      for (i = start; i <= end; i++) if (has_framing(lines[i])) exit 0

      # Only a code block containing retired flags may borrow framing from its
      # immediately preceding prose paragraph. A document header mentioning
      # "historical" does not frame ordinary prose elsewhere in that document.
      if (!in_fenced_code(target)) exit 1

      # Skip only blank lines and the code-block opening fence.
      previous = start - 1
      while (previous > 0 && blank(lines[previous])) previous--
      if (lines[previous] ~ /^[[:space:]]*```/) {
        previous--
        while (previous > 0 && blank(lines[previous])) previous--
      }
      if (previous == 0) exit 1
      prior_start = previous
      while (prior_start > 1 && !blank(lines[prior_start - 1])) prior_start--
      for (i = prior_start; i <= previous; i++) if (has_framing(lines[i])) exit 0
      exit 1
    }
  ' "$file"; then
    printf 'stale matcher documentation: %s:%s\n' "$file" "$line" >&2
    status=1
  fi
done < <(rg -n -i "${retired}" --glob '*.md' "${roots[@]}" || true)

# The obsolete baseline filename is stronger than a retired term used in
# explanatory prose: active documentation must never route current evidence to
# it. Only a document whose first non-empty line is the canonical historical
# marker may retain the link as part of its dated record.
while IFS=: read -r file line _; do
  if awk '
    BEGIN { status = 1 }
    /^[[:space:]]*$/ { next }
    /^>[[:space:]]*\*\*Historical record — .+\*\*[[:space:]]*$/ {
      status = 0
      exit
    }
    { exit }
    END { exit status }
  ' "$file"; then
    continue
  fi
  printf 'obsolete current-evidence link: %s:%s\n' "$file" "$line" >&2
  status=1
done < <(
  rg -n '2026-07-12-supported-cache-baseline(\.md)?' \
    --glob '*.md' "${roots[@]}" || true
)

# These are the active contributor/performance entry points. Unlike preserved
# historical material, none may advertise a deleted benchmark target. Keeping
# this list explicit avoids treating an archived result as a current command.
readonly -a current_benchmark_docs=(
  AGENTS.md CLAUDE.md README.md CONTRIBUTING.md lingrep/README.md
  docs/cli-usage-guide.md docs/PERFORMANCE_GUIDE.md
  docs/development-environment.md docs/PERFORMANCE_REQUIREMENTS.md
  docs/testing-guide.md lingrep-core/benches/README.md
)
while IFS=: read -r file line _; do
  printf 'deleted benchmark in current guidance: %s:%s\n' "$file" "$line" >&2
  status=1
done < <(
  rg -n -i "$deleted_targets" --glob '*.md' "${current_benchmark_docs[@]}" || true
)

# Current contributor entry points must name the current matcher, query
# contract, and source-tree layout. Historical documents are deliberately not
# included: their old names are evidence rather than instructions.
readonly stale_current_surface='CompiledFsm|SimdStringMatcher|add_pattern\(|vp-search\.query/v[1-5]([^0-9]|$)|compressed or uncompressed token-cache|Dictionary-backed matching representation|Exact indexes are a separate capability|lingrep/src/tui\.rs|CACHE_FORMATS\.md|BENCHMARK_RESULTS\.md|ling (schema|corpus sources)([^A-Za-z-]|$)|--explain([^A-Za-z-]|$)|attributes --json|\[(pos|cform)=|/unidic-[^[:space:]]+\.dic\.zst|<name>-202512\.dic\.zst|ling-tsv-v1\.md'
readonly -a current_surface_docs=(
  AGENTS.md README.md CONTRIBUTING.md lingrep/README.md
  scripts/README.md benchmarks/harbor/guides/analyzer-guide.md
  lingrep-core/benches/README.md docs/README.md docs/cli-usage-guide.md
  docs/PERFORMANCE_GUIDE.md docs/PERFORMANCE_REQUIREMENTS.md
  docs/development-environment.md docs/testing-guide.md docs/tui-guide.md
  docs/deployment-guide.md docs/fsm-development.md docs/parser-limitations.md
  docs/pattern-language-co-development.md docs/pattern-language-reference.md
)
while IFS= read -r hit; do
  printf 'stale current surface: %s\n' "$hit" >&2
  status=1
done < <(rg -n "$stale_current_surface" "${current_surface_docs[@]}" || true)

# The Harbor guide is copied into the evaluation image. Unlike explanatory
# prose in AGENTS.md, any retired field named here becomes direct model input.
while IFS= read -r hit; do
  printf 'stale Harbor token attribute: %s\n' "$hit" >&2
  status=1
done < <(
  rg -n 'unidic\.text' benchmarks/harbor/guides/analyzer-guide.md || true
)

readonly retired_prepared='tokens_v6|tokens\.vp6([^0-9]|$)|V6SentenceSource|TokenV6(Reader|Writer)|TokenEncoding|--encoding[ =]|Uncompressed cache matching'
readonly -a current_prepared_docs=(
  AGENTS.md CLAUDE.md README.md CONTRIBUTING.md lingrep/README.md
  docs/cli-usage-guide.md
  docs/development-environment.md docs/PERFORMANCE_GUIDE.md
  docs/testing-guide.md docs/tui-guide.md
)

while IFS= read -r hit; do
  printf 'stale prepared-storage guidance: %s\n' "$hit" >&2
  status=1
done < <(rg -n -i "$retired_prepared" "${current_prepared_docs[@]}" || true)

while IFS= read -r hit; do
  printf 'stale prepared-storage implementation: %s\n' "$hit" >&2
  status=1
done < <(
  rg -n "$retired_prepared" \
    --glob '*.rs' --glob '*.sh' --glob '*.toml' \
    --glob '!scripts/audit-matcher-cutover-docs.sh' \
    --glob '!lingrep-corpus/src/lib.rs' \
    lingrep-corpus/src lingrep/src lingrep-web/src ci.sh scripts || true
)

# Retired implementation surfaces must not return in source or tools. Dated
# plans and results are records, not build inputs, and are deliberately absent.
readonly retired_cache_surface='lingrep_core::cache|crate::cache|memory_calculator|fsm::corpus_view|test_utils|DictionaryEncodedCache|ChunkedCache(Reader|Writer)'
while IFS= read -r hit; do
  printf 'retired cache implementation: %s\n' "$hit" >&2
  status=1
done < <(
  rg -n "$retired_cache_surface" \
    --glob '*.rs' --glob '*.sh' --glob '*.toml' \
    --glob '!scripts/audit-matcher-cutover-docs.sh' \
    lingrep-core/src lingrep-corpus/src lingrep/src lingrep-web/src lingrep-cli-common/src lingrep-perf/src tools \
    2>/dev/null || true
)

# Workflows are live launchers, not historical records: framing cannot waive a
# removed executable or benchmark name here.
while IFS= read -r hit; do
  printf 'stale matcher workflow launcher: %s\n' "$hit" >&2
  status=1
done < <(
  rg -n -i "vp-pattern-matcher|vp_pattern_matcher|$deleted_targets" \
    --glob '*.yml' --glob '*.yaml' .github || true
)

# Source and script launchers are not documentation and cannot be waived with
# prose framing. This audit script names the forbidden spelling too.
while IFS= read -r hit; do
  printf 'stale matcher launcher: %s\n' "$hit" >&2
  status=1
done < <(
  rg -n 'vp-pattern-matcher|vp_pattern_matcher' \
    --glob '*.rs' --glob '*.sh' \
    --glob '!scripts/audit-matcher-cutover-docs.sh' \
    lingrep-core/tests scripts || true
)

# Resolve every live, package-qualified benchmark launcher against Cargo's
# declared targets. The validator also carries negative fixtures for deleted,
# dynamic, unqualified, and retired-executable launchers.
if ! python3 scripts/check-live-launchers.py; then
  status=1
fi

# A durable artifact generation must not be documented as rebuild-only while a
# durability fixture still claims this package reads it. Documentation and code
# disagreeing about that is how a reader learns to rebuild a corpus that is
# perfectly readable.
#
# Only current guides participate: dated plans belong to the pre-lingrep
# namespace, where a generation number can have a different meaning. Current
# artifact generation, token format, manifest schema and identity scheme are
# independent axes; this check uses artifact generation alone.
if ! python3 - <<'DURABILITY'; then
import pathlib
import re
import sys

fixtures = pathlib.Path("lingrep-corpus/tests/fixtures/durability")
retained = sorted(
    {
        match.group(1)
        for entry in (fixtures.iterdir() if fixtures.is_dir() else [])
        if entry.is_dir() and (match := re.match(r"^g(\d+)-", entry.name))
    }
)
if not retained:
    print("error: no durable generation is retained; nothing to audit",
          file=sys.stderr)
    raise SystemExit(1)

current = [
    "AGENTS.md", "CLAUDE.md", "README.md", "CONTRIBUTING.md",
    "lingrep/README.md", "docs/cli-usage-guide.md",
    "docs/PERFORMANCE_GUIDE.md", "docs/development-environment.md",
    "docs/PERFORMANCE_REQUIREMENTS.md", "docs/testing-guide.md",
    "docs/deployment-guide.md",
]

# "generation 6", "artifact generation 6", "g6", ".g6." — the address spelling
# and the prose spelling.
generation = re.compile(
    r"(?:artifact\s+)?generation\s+(\d+)|(?<![A-Za-z0-9])g(\d+)(?![A-Za-z0-9])",
    re.IGNORECASE,
)
# Only unambiguous retirement language. "No compatibility reader" is
# deliberately absent: it is also how a correct sentence describes a *changed
# identity* getting a fresh artifact, which says nothing about whether a
# generation is readable.
retirement = re.compile(
    r"rebuild[- ]only|must be rebuilt|unreadable by|no longer readable"
    r"|are unsupported|is unsupported",
    re.IGNORECASE,
)

# The scope is a **sentence**, not a line. A line may legitimately mention a
# retained generation and a retired one at once — "fixtures start at generation
# 6; generation 5 and older were already retired rebuild-only" is exactly that
# shape, and it is correct. Scoping to the line would fail it.
sentence_break = re.compile(r"(?<=[.;:])\s+|\n\n")

status = 0
for name in current:
    path = pathlib.Path(name)
    if not path.is_file():
        continue
    text = path.read_text(encoding="utf-8")
    offset = 0
    for sentence in sentence_break.split(text):
        start = text.find(sentence, offset)
        if start >= 0:
            offset = start + len(sentence)
        if retirement.search(sentence):
            for match in generation.finditer(sentence):
                found = match.group(1) or match.group(2)
                if found not in retained:
                    continue
                line = text.count("\n", 0, start) + 1
                print(
                    f"durable generation documented as retired: {name}:{line}\n"
                    f"       generation {found} retains "
                    f"lingrep-corpus/tests/fixtures/durability/g{found}-*, so this "
                    "package reads it.\n"
                    "       Either the sentence is wrong, or the fixture is a "
                    "retirement that needs a migration test.",
                    file=sys.stderr,
                )
                status = 1
raise SystemExit(status)
DURABILITY
  status=1
fi

if (( status != 0 )); then
  printf '%s\n' 'Use the current ling and FsmBuilder -> CompiledCatalog contracts or mark the material historical.' >&2
  exit "$status"
fi

printf '%s\n' 'Matcher cutover documentation audit passed.'
