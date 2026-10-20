# Matcher and execution benchmarks

Run benchmarks only after reading
[Performance Requirements](../../docs/PERFORMANCE_REQUIREMENTS.md). Record a
baseline for the supported production path before using a result to justify a
change.

```bash
cargo bench
```

The supported matcher is immutable `CompiledCatalog` NFA matching over bounded
sentence batches. Use `fsm_performance` for matcher work, `corpus_discovery`
for recursive admission, `scope_admission` for scope evaluation, and
`spill_codec` for the shared spill representation. Prepared-artifact benchmarks
belong to `lingrep-corpus`'s `prepared_tokens` target.

Acceptance criteria and campaign selection live in
[Performance Requirements](../../docs/PERFORMANCE_REQUIREMENTS.md).
