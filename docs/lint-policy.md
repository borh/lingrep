# Workspace Clippy lint policy

The lint policy is `[workspace.lints.clippy]` in the root `Cargo.toml`; every
member crate opts in with `[lints] workspace = true`. `clippy.toml` holds the
test allowances and thresholds. Test, benchmark and example files outside inline
`#[cfg(test)]` modules use a per-file exemption; see
[Test-target exemption](#test-target-exemption).

## The `deny` set is not a panic-free guarantee

These lints are `deny`, so a bare `cargo clippy` fails on them without CI's
`-D warnings`:

- `string_slice`
- `panic`, `panic_in_result_fn`, `unreachable`, `unimplemented`, `todo`,
  `exit`, `unchecked_time_subtraction`
- `unwrap_used`
- the `pedantic` group, minus the allow-list in `Cargo.toml`
- the pedantic lints listed individually as `deny` in `Cargo.toml`

A production site that must trip one of them carries a reasoned `#[expect]`
stating the invariant.

Panics remain possible from indexing and slicing (`indexing_slicing`),
arithmetic overflow and division by zero (`arithmetic_side_effects`),
`.expect()` (`expect_used`), and macros outside the workspace. None of these is
denied.

Default-on Clippy lints are not listed in `Cargo.toml`: they keep Clippy's
level, and CI's `-D warnings` turns their warnings into errors. Do not add
`= "allow"` for a default-on lint, because that lowers its enforcement.

## The `allow` entries

| Lint | Reason for `allow` |
|---|---|
| `expect_used` | Production `.expect("msg")` states an invariant in constructors and accessors that do not return `Result`, so `?` does not apply. Denying it would require an `#[expect(reason = "X")]` beside every `.expect("X")`. |
| `get_unwrap` | Every `.get(i).unwrap()` already trips `unwrap_used`, and the suggested fix, indexing, still panics. |
| `unwrap_in_result` | Also fires on `.expect()`, which conflicts with `expect_used = "allow"`; the `.unwrap()` case is covered by `unwrap_used`. |

These are default-off `restriction` lints; the entries record the decision
where the rule is configured.

`.unwrap()` is denied and `.expect("msg")` is allowed, although both panic. Do
not convert `.unwrap()` to `.expect("msg")` only to satisfy `unwrap_used`.

### Pedantic allow-list

The allow-list overrides `pedantic = deny` for two kinds of lint, both listed
in `Cargo.toml`:

- policy overrides that stay allowed even when the code is clean:
  `must_use_candidate`, `missing_errors_doc`, `missing_panics_doc`,
  `needless_pass_by_value`, `similar_names`, `float_cmp`, `cast_sign_loss`,
  `invalid_upcast_comparisons`, `large_futures`,
  `iter_not_returning_iterator`, `expl_impl_clone_on_copy`;
- lints with existing findings that have not been fixed.

Removing an entry makes `pedantic = deny` fail on every existing site. To move a
lint from the allow-list to `deny`, fix every site (`cargo clippy --fix` for
machine-applicable suggestions, then `cargo fmt` and the affected tests) and
delete its `allow` line in the same commit. Do not delete a policy override
because it measures clean.

`redundant_else` and `unused_async_trait_impl` fire on the flake's nightly
Clippy and are allow-listed rather than fixed. Stable Clippy does not recognize
`unused_async_trait_impl` and reports it as an `unknown_lints` warning.

## No `warn` level in `Cargo.toml`

With CI's `-D warnings`, a `warn` entry behaves as `deny` in CI but not locally.
`Cargo.toml` therefore lists each lint as `deny` or `allow` only. A commit that
elevates a lint fixes its sites and sets `deny` in the same commit.

## Test-target exemption

The `allow-*-in-tests` keys in `clippy.toml` exempt inline `#[cfg(test)]`
modules in `src/`. They do not exempt integration tests (`tests/`), benchmarks
(`benches/`), or examples (`examples/`).

Each file there that trips a deny lint starts with a combined
`#![allow(clippy::string_slice, clippy::panic, …, clippy::unwrap_used)]`. Only
files that trip a lint carry it. If a change adds a bare `.unwrap()`, `panic!`,
`unreachable!` or `&s[a..b]` to a test, benchmark or example file without the
header, CI fails; add the header, copying a sibling file, or use a
non-panicking form. `.expect("msg")` requires no header.

## Lints absent from `Cargo.toml`

A default-off `restriction` or `nursery` lint that no enabled group turns on is
left out rather than listed as `allow`: `indexing_slicing`,
`arithmetic_side_effects`, `as_conversions`, `mem_forget`, `float_cmp_const`,
`lossy_float_literal`, `rc_mutex`, `dbg_macro`, `allow_attributes`,
`allow_attributes_without_reason`. Adding one requires fixing its findings
first. Enabling the `nursery` group requires a `rust-toolchain.toml` pin in the
same change.

## Enforcement and toolchains

The enforcing gate is the `fast` job in `.github/workflows/ci.yml`, which runs
the lane on the flake's nightly toolchain (`rust-overlay`'s `nightly.latest`,
pinned by `flake.lock`). Run the same command locally:

```bash
nix develop .#ci -c ./ci.sh --fast
```

`.githooks/pre-push` runs the same lane through the default shell, which uses
the same toolchain. The lane cannot run on stable, because
`scripts/check-test-thread-default.sh` uses `cargo config get`, which is
nightly-only. The `macos` CI job also runs stable Clippy with `-D warnings`.
`.#stable` and `.#msrv` are compatibility shells for checking that the code
builds on those toolchains.
