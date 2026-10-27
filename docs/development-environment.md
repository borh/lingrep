# Development environment

The supported development environment is the repository's Nix flake. It
provides Rust, native build dependencies, dictionaries, JavaScript tooling,
Chromium/ChromeDriver, and the utilities used by repository scripts.

## Start a shell

```bash
nix develop
```

The default shell uses the Rust toolchain that `rust-toolchain.toml` pins,
which is also the toolchain CI and release builds use; rustup reads the same
file outside Nix. A commit that updates the pin also fixes the new compiler
and Clippy warnings the updated toolchain reports.
Six shells are available:

| Shell | Purpose |
| --- | --- |
| `nix develop` | Daily development and repository gates, on the pinned toolchain |
| `nix develop .#nightly` | Sanitizer fuzzing (`LINGREP_FUZZ_SANITIZER=address`) |
| `nix develop .#stable` | The newest stable in the locked `rust-overlay` |
| `nix develop .#msrv` | Workspace MSRV compatibility |
| `nix develop .#ci` | The GitHub workflow's shell: the pinned toolchain and only what `./ci.sh --fast` invokes |
| `nix develop .#packaged` | User workflows: the default shell plus the packaged `ling` and `lingrep-web` on `PATH` |

`.#ci` omits Chromium and ChromeDriver, `perf`, `gdb`, `valgrind`, and the
ROCm stack so that it fits on a CI runner alongside the CWJ dictionary and a
Cargo target directory. Use the default shell for profiling and browser checks.

## Build and run

Inside the shell:

```bash
cargo build --workspace
cargo build -p lingrep --release
cargo run -p lingrep -- --help
```

Use the packaged application when validating Nix output, installed completions,
or generated manual pages:

```bash
nix run .#lingrep -- --help
nix build .#lingrep
./result/bin/ling --help
```

`nix run .#lingrep-web -- --help` exercises the separately packaged browser
application.

## Dictionaries

`nix develop` exports `LINGREP_DICTIONARY_PATH` for the default CWJ dictionary.
Build or select another supported UniDic variant as described in
[DICTIONARIES.md](../DICTIONARIES.md).

```bash
dict_root="$(nix build .#vibrato-dictionaries --no-link --print-out-paths)"
export LINGREP_DICTIONARY_PATH="$dict_root/share/vibrato/unidic-cwj-202512.dic"
```

Direct corpus commands require a dictionary. Prepared `.lingcorpus` artifacts
already contain analyzed tokens and reject a dictionary override.

## Run the GiNZA command line on a GPU

`python/spacy-adapter/bin/ginza-gpu <rocm|cuda> <ginza arguments>` runs the
stock GiNZA command line on an AMD or NVIDIA GPU. Both backends have been
verified against CPU output. The `lingrep-spacy-adapter` adapter initializes
the GPU libraries itself and does not require this launcher.

1. Select `rocm` for AMD or `cuda` for NVIDIA and sync its extra into a
   separate environment. The GPU and CPU extras select different Torch builds
   and cannot be installed together:

   ```bash
   backend=rocm
   gpu_env=/path/to/venv-gpu
   UV_PROJECT_ENVIRONMENT="$gpu_env" \
     uv sync --project python/spacy-adapter --frozen \
     --extra "$backend" --extra spacy --extra ginza
   ```

2. Run the launcher inside `nix develop`, which supplies `hipcc` for ROCm
   kernel compilation, and select the device with GiNZA's `-g` flag:

   For CUDA on NixOS, include the installed NVIDIA driver in the library
   search path before running the launcher:

   ```bash
   export LD_LIBRARY_PATH=/run/opengl-driver/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
   ```

   ```bash
   nix develop -c "$gpu_env/bin/python" \
     python/spacy-adapter/bin/ginza-gpu "$backend" \
     -g 0 -d -c print -f conllu sentences.txt > sentences.conllu
   ```

`LINGREP_SPACY_RUN_CUDA=1 scripts/check-spacy-adapter.sh full` compares the
launcher's complete CoNLL-U output with CPU execution on a Japanese fixture.
For ROCm, set `LINGREP_SPACY_RUN_ROCM=1` and
`LINGREP_SPACY_GINZA_MODEL=ja_ginza`; that lane also checks the Rust-owned
adapter's CPU/GPU parity.

Two ROCm failures have a known cause. `ValueError: Cannot use GPU, CuPy is not
installed` means CuPy was imported before Torch: the ROCm CuPy wheel loads its
HIP runtime from libraries bundled with the Torch wheel, and thinc probes CuPy
once at import time, so plain `ginza -g 0` fails. spaCy error E050 (`You need to install "ja-ginza"`) with `ja-ginza`
installed means CuPy could not find `hipcc`; re-run under `nix develop`.

## Test and validate

Use focused tests while iterating:

```bash
cargo test -p lingrep-core --test suite -- <module-or-test-name>
cargo test -p lingrep --test suite -- <module-or-test-name>
cargo test -p lingrep-web --lib <module-or-test-name>
```

The repository validation lanes are:

```bash
./ci.sh --fast         # routine and pre-push correctness
./ci.sh --full         # complete branch-finish correctness
./ci.sh --fuzz         # bounded parser/matcher fuzz smoke
./ci.sh --performance  # selected performance evidence
```

Bare `./ci.sh` is the same as `--full`.

When changing public Rust documentation, run the relevant rustdoc lane too:

```bash
RUSTDOCFLAGS="-D warnings" cargo doc -p <crate> --no-deps
```

## Web development

SQLx query macros compile against the committed `.sqlx` metadata; ordinary
builds do not require a live database. Refresh the browser's lingrep patterns
assets only when the pinned grammar changes:

```bash
scripts/refresh-pattern-web-assets.sh
```

Run JavaScript tests and the browser layout check with the tools supplied by
the Nix shell:

```bash
node --test lingrep-web/js-tests/*.test.mjs
scripts/run_lingrep_web_layout_e2e.sh
```

The layout runner exits successfully without checking anything when Chromium
or ChromeDriver is not on `PATH`, so run it inside `nix develop`.

## Performance work

Read [PERFORMANCE_REQUIREMENTS.md](PERFORMANCE_REQUIREMENTS.md) before changing
a performance-sensitive path. It specifies the measurement tiers, targets, and
campaign workflow.
