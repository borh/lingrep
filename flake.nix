{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane = {
      url = "github:ipetkov/crane/v0.24.0";
    };
    mecab-dic-converter-src = {
      url = "github:tomokane/mecab-dic-converter/d24dcf25ce47170ca9e661c003b5d3345e98dac9";
      flake = false;
    };
    sudachi-rs-src = {
      url = "github:WorksApplications/sudachi.rs/f4dd8f20a774bd71d34a7d4ffa00d987b8946f9e";
      flake = false;
    };
  };

  outputs =
    inputs:
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      # Darwin is a supported development platform; Linux is the deployment
      # one. Several dev-shell packages and two checks are Linux-only in
      # substance, and an unsupported package throws during *evaluation* rather
      # than at build time, so each is gated explicitly below rather than left
      # to `meta.platforms`.
      systems = [
        "x86_64-linux"
        "aarch64-darwin"
      ];

      # Cross-cutting outputs that are not per-system live here. The lingrep-web
      # NixOS module imports the option schema and the activator; the smoke
      # test sits under `checks.x86_64-linux.lingrep-web-nixos-smoke` below so
      # `nix flake check --no-build` reaches every new gate.
      flake = {
        nixosModules.lingrep-web = {
          imports = [
            ./nix/modules/lingrep-web/options.nix
            ./nix/modules/lingrep-web/nixos.nix
          ];
        };
      };

      perSystem =
        {
          config,
          self',
          pkgs,
          lib,
          system,
          ...
        }:
        let
          isLinux = pkgs.stdenv.hostPlatform.isLinux;

          runtimeDeps = [ ];
          buildDeps = with pkgs; [
            pkg-config
            openssl
          ];
          # Available on both platforms. `util-linux` stays here because it is
          # what supplies `flock(1)`, which the ling test harness spawns.
          portableDevDeps = with pkgs; [
            bash-completion
            bashInteractive
            elvish
            fish
            time
            util-linux
            nodejs_24
            jq
            sqlx-cli
            tree-sitter
            uv
            python314
            cargo-sweep
            just
            zsh
          ];
          # Each of these throws on evaluation for darwin — `meta.unsupported`
          # for perf/chromium/ROCm — or has no working macOS backend. `mold` is
          # the exception that still *evaluates*: nixpkgs lists darwin in its
          # platforms, but upstream withdrew Mach-O support, so selecting it
          # would fail at link time instead. ROCm has no Apple Silicon analogue
          # at all.
          linuxOnlyDevDeps = with pkgs; [
            gdb
            perf
            chromium
            chromedriver
            rocmPackages.clr
            rocmPackages.hipcub
            rocmPackages.rocprim
            rocmPackages.rocm-device-libs
            rocmPackages.rocthrust
            mold
            stdenv.cc.cc.lib
          ];
          darwinOnlyDevDeps = with pkgs; [ lldb ];

          devDeps =
            portableDevDeps
            ++ lib.optionals isLinux linuxOnlyDevDeps
            ++ lib.optionals (!isLinux) darwinOnlyDevDeps;

          msrv = "1.95.0";

          craneLib = inputs.crane.mkLib pkgs;

          src = lib.cleanSourceWith {
            name = "lingrep-src";
            src = ./.;
            filter =
              path: type:
              let
                ext = lib.last (lib.splitString "." path);
                isHtml = ext == "html";
                isSgx = ext == "pattern";
                # lingrep-core compiles the checked-in NDC9 lookup table into the
                # BCCWJ reader with include_str!.
                isNdc9Data = toString path == toString ./lingrep-core/src/corpus/bccwj/data/ndc9.tsv;
                # lingrep-web include_bytes!s assets under lingrep-web/assets (both the pattern
                # subdir and top-level results-explorer.js / app.css), so keep the
                # whole tree, not just assets/editor.
                isWebAsset = lib.hasPrefix "${toString ./lingrep-web/assets}/" (toString path);
                # lingrep-web's sqlx::query_as macros compile against this committed
                # offline cache; filterCargoSources drops it, breaking any
                # hermetic build of lingrep-web.
                isSqlxCache = lib.hasPrefix "${toString ./.sqlx}/" (toString path);
              in
              isHtml || isSgx || isNdc9Data || isWebAsset || isSqlxCache || craneLib.filterCargoSources path type;
          };

          cargoVendorDir = craneLib.vendorCargoDeps {
            inherit src;
            overrideVendorGitCheckout =
              packages: checkout:
              if lib.any (package: package.name == "sudachi") packages then
                checkout.overrideAttrs (old: {
                  postInstall = (old.postInstall or "") + ''
                    cp -R ${inputs.sudachi-rs-src}/resources "$out/resources"
                  '';
                })
              else
                checkout;
          };

          commonArgs = {
            pname = "lingrep";
            # Read from the workspace manifest rather than restated here: the
            # package, the crates and `ling --version` cannot then name
            # different versions, which is how the workspace sat at 0.1.0 while
            # the newest tag was v0.2.3.
            version =
              (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
            inherit src;
            inherit cargoVendorDir;
            strictDeps = true;

            nativeBuildInputs = buildDeps;
            buildInputs = runtimeDeps ++ [ pkgs.openssl ];

            je_cv_strerror_r_returns_char_with_gnu_source = "yes";
            GIT_FETCH_WITH_CLI = "true";

            # openssl-sys (transitive dep) needs these in the Nix sandbox
            OPENSSL_DIR = "${pkgs.openssl.dev}";
            OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";

            doCheck = false;
          };

          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          mkLingrepPackage =
            {
              features ? "",
              cargoExtraArgs ? "",
              postInstall ? "",
            }:
            craneLib.buildPackage (
              commonArgs
              // {
                inherit cargoArtifacts;
                cargoExtraArgs = lib.concatStringsSep " " (
                  lib.filter (argument: argument != "") [
                    cargoExtraArgs
                    (lib.optionalString (features != "") "--features ${features}")
                  ]
                );
                inherit postInstall;
              }
            );

          # The toolchain must be threaded through `overrideToolchain`, not just
          # appended to `packages`. `craneLib.devShell` puts its own toolchain in
          # `nativeBuildInputs`, which precede `packages` on PATH, so a toolchain
          # added to `packages` is shadowed: every shell silently served the
          # nixpkgs rustc and `.#nightly` / `.#msrv` were indistinguishable from
          # `.#stable`.
          mkDevShellWith =
            {
              rustToolchain,
              packages,
              # ROCm is a profiling extra: it is multi-gigabyte and nothing in
              # `./ci.sh --fast` links against it. Its exports are interpolated
              # only when this is set, because forcing a ROCm derivation during
              # evaluation is what threw on darwin before they were gated.
              profilingExtras ? true,
            }:
            (craneLib.overrideToolchain rustToolchain).devShell (
              # `version` is dropped deliberately. stdenv turns a derivation's
              # `version` attribute into a shell variable, so `nix print-dev-env`
              # emitted `version='0.1.0'` and direnv applied it to the caller's
              # shell. In fish `$version` is the read-only interpreter version,
              # so every direnv entry failed with `set: Tried to change the
              # read-only variable 'version'`. Nothing in a dev shell reads it;
              # the package builds that do still take it from `commonArgs`.
              (builtins.removeAttrs commonArgs [ "version" ])
              // {
                inherit packages;

                # Single dictionary path for the whole workspace: the CLI's
                # --dict-path default, the test helper `lingrep_core::testing::
                # dictionary_path()`, and the real-workload benchmarks all read
                # this one variable, so nothing needs a manual export in-shell.
                LINGREP_DICTIONARY_PATH = "${vibratoDictCwj}/share/vibrato/unidic-cwj-202512.dic";
                LINGREP_SUDACHI_RESOURCES = "${sudachiRs}/share/sudachi/resources";
                LINGREP_SUDACHI_DICT_SMALL = "${sudachiDictSmall}/share/sudachi/system_small.dic";
                LINGREP_SUDACHI_DICT_CORE = "${sudachiDictCore}/share/sudachi/system_core.dic";
                LINGREP_SUDACHI_DICT_FULL = "${sudachiDictFull}/share/sudachi/system_full.dic";
                LINGREP_BASH_COMPLETION_PATH = "${pkgs.bash-completion}/share/bash-completion/bash_completion";
                LINGREP_FISH_PATH = "${pkgs.fish}/bin/fish";

                # mold replaces GNU ld. Rebuilds were link-bound, not compute-
                # bound: sys 3m22 vs user 2m28 on a full test-binary rebuild.
                # darwin keeps the default ld64: mold never gained a Mach-O
                # backend that survived, and ld64 is already the fast linker
                # there. `scripts/check-build-env.sh` enforces the same split.
                RUSTFLAGS = lib.optionalString isLinux "-C link-arg=-fuse-ld=mold";

                # Incremental compilation, deliberately overriding the
                # CARGO_INCREMENTAL=0 and RUSTC_WRAPPER=sccache that arrive from
                # the systemd user environment. sccache cannot cache incremental
                # builds, and its measured hit rate here was 24.4%. The global
                # settings are left alone; this override is repo-local.
                CARGO_INCREMENTAL = "1";
                RUSTC_WRAPPER = "";

                shellHook = ''
                  if command -v sccache > /dev/null 2>&1; then
                    # sccache creates a Unix startup-notification socket beneath TMPDIR.
                    # NIMAS session TMPDIR paths can exceed the socket-path limit.
                    export TMPDIR=/tmp
                    export TMP="$TMPDIR"
                    export TEMPDIR="$TMPDIR"
                  fi
                  # Keep rust-analyzer on the source tree that matches the
                  # shell's selected toolchain.
                  export RUST_SRC_PATH=${rustToolchain}/lib/rustlib/src/rust/library
                ''
                # Every export below names a Linux-only store path, so the
                # interpolations must sit *inside* the conditional: forcing a
                # ROCm derivation on darwin throws during evaluation, which
                # gating only `devDeps` would not have prevented.
                #
                # `je_cv_…` is a jemalloc autoconf cache variable about glibc's
                # GNU strerror_r, and LD_LIBRARY_PATH has no darwin meaning —
                # DYLD_LIBRARY_PATH is not a substitute, since SIP strips it
                # from system binaries.
                + lib.optionalString isLinux ''
                  export je_cv_strerror_r_returns_char_with_gnu_source=yes
                  export LD_LIBRARY_PATH=${
                    lib.makeLibraryPath [
                      pkgs.stdenv.cc.cc.lib
                      pkgs.zlib
                    ]
                  }''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
                ''
                + lib.optionalString (isLinux && profilingExtras) ''
                  export HIP_PATH=${pkgs.rocmPackages.clr}
                  export ROCM_PATH=${pkgs.rocmPackages.clr}
                  export HIP_DEVICE_LIB_PATH=${pkgs.rocmPackages.rocm-device-libs}/amdgcn/bitcode
                  export CPLUS_INCLUDE_PATH=${pkgs.rocmPackages.rocthrust}/include:${pkgs.rocmPackages.rocprim}/include:${pkgs.rocmPackages.hipcub}/include''${CPLUS_INCLUDE_PATH:+:$CPLUS_INCLUDE_PATH}
                '';
              }
            );

          # The full development shell: profiling, browser and GPU extras
          # included, because that is what a developer reaches for and what
          # `.githooks/pre-push` enters.
          mkDevShell =
            rustToolchain:
            mkDevShellWith {
              inherit rustToolchain;
              packages =
                buildDeps
                ++ runtimeDeps
                ++ devDeps
                ++ [ pkgs.cargo-fuzz ]
                # valgrind is marked broken on aarch64-darwin — there is no
                # working arm64-macOS port — so it joins the gated set rather
                # than the common one.
                ++ lib.optionals isLinux [ pkgs.valgrind ];
            };

          # `devShells.ci` exists because a GitHub runner has to hold the shell,
          # the 1.31 GB CWJ dictionary and a cargo target directory at the same
          # time, and the full shell does not fit. Chromium and chromedriver
          # belong to the browser layout checks in the full lane; perf, gdb,
          # valgrind and the ROCm stack belong to profiling. `sqlx-cli` goes too:
          # the committed `.sqlx` cache with `SQLX_OFFLINE=true` is what the build
          # reads, and the tool only regenerates that cache. Everything
          # `./ci.sh --fast` invokes stays, including mold, which
          # `scripts/check-build-env.sh` requires on Linux.
          ciDevDeps =
            buildDeps
            ++ runtimeDeps
            ++ (lib.filter (package: package != pkgs.sqlx-cli) portableDevDeps)
            ++ lib.optionals isLinux [ pkgs.mold ];

          mecabDicConverter = pkgs.rustPlatform.buildRustPackage {
            pname = "mecab-dic-converter";
            version = "0.1.0";
            src = inputs.mecab-dic-converter-src;
            patches = [ ./nix/patches/mecab-dic-converter-vibrato-rkyv-0.7.12.patch ];
            postPatch = ''
              cp ${./nix/mecab-dic-converter/Cargo.lock} Cargo.lock
            '';
            cargoLock = {
              lockFile = ./nix/mecab-dic-converter/Cargo.lock;
              outputHashes = {
                "vibrato-rkyv-0.7.12" = "sha256-fe52UCDFvHQ2UHfwOTcua4HL3eAdiT27nb4j5iLsF34=";
              };
            };
            buildFeatures = [ "vibrato-export" ];
            cargoTestFlags = [
              "--features"
              "vibrato-export"
            ];
          };

          buildUnidicVibratoDict =
            {
              name,
              url,
              hash,
            }:
            let
              unidicSrc = pkgs.fetchzip {
                inherit url hash;
                name = "${name}-src";
                stripRoot = false;
              };
            in
            pkgs.runCommand "vibrato-dict-${name}-202512"
              {
                nativeBuildInputs = [
                  mecabDicConverter
                  pkgs.zstd
                ];
              }
              ''
                mkdir -p "$out/share/vibrato"
                target="$out/share/vibrato/${name}-202512.dic"

                # Ship the dictionary uncompressed.
                #
                # A compressed dictionary does not replace its decompressed
                # form, it *adds* to it: the loader expands the archive into
                # `~/.cache/vibrato-rkyv` on first use and memory-maps that
                # copy from then on. So compressing costs the archive *plus*
                # the expansion — for cwj, 419 MB plus 1.31 GB against 1.31 GB
                # of bytes — and buys a decompression, a cache directory, and
                # the ways both can fail. Uncompressed is 24% less disk and a
                # cold load falls from 0.91 s to 0.48 s with the page cache
                # evicted too. It costs a larger store: 17.3 GB across the
                # seven against 5.3 GB, which is the wrong trade only for a
                # dictionary that is built and never loaded. See
                # docs/results/2026-08-24-parallel-dictionary-decompression.
                #
                # A NAR is compressed for transfer, so this costs roughly the
                # archive's bytes to substitute either way; what changes is
                # local store size.
                mecab-dic-converter build-vibrato \
                  --dictionary-root ${unidicSrc} \
                  --output built.dic.zst

                zstd -q -d -c built.dic.zst > "$target"
                rm -f built.dic.zst

                # The digest of these bytes reaches every prepared artifact's
                # analysis identity, and the loader refuses anything whose
                # magic it does not recognize. Check both here rather than at
                # first use, where the failure is a user's query.
                magic=$(head -c 24 "$target")
                if [ "$magic" != "VibratoTokenizerRkyv 0.6" ]; then
                  echo "unexpected dictionary magic: $magic" >&2
                  exit 1
                fi
              '';

          vibratoDictCwj = buildUnidicVibratoDict {
            name = "unidic-cwj";
            url = "https://clrd.ninjal.ac.jp/unidic_archive/2512/unidic-cwj-202512.zip";
            hash = "sha256-lNvcTlhqSWoLPF104eS7r08Dw5SQYRwzTQXy6DxdfjM=";
          };
          vibratoDictCsj = buildUnidicVibratoDict {
            name = "unidic-csj";
            url = "https://clrd.ninjal.ac.jp/unidic_archive/2512/unidic-csj-202512.zip";
            hash = "sha256-W0toSrgD8R+KLWos1XuaR3qJhiiYLXtwyd1sUvuYFAM=";
          };
          vibratoDictNovel = buildUnidicVibratoDict {
            name = "unidic-novel";
            url = "https://clrd.ninjal.ac.jp/unidic_archive/2512/unidic-novel-v202512.zip";
            hash = "sha256-19wqA64F2CJFYaHzZTMCxOcwYoOTd6C98GHa5b/RUH8=";
          };
          vibratoDictQkana = buildUnidicVibratoDict {
            name = "unidic-qkana";
            url = "https://clrd.ninjal.ac.jp/unidic_archive/2512/unidic-qkana-v202512.zip";
            hash = "sha256-bzlybu2NBEFCsfTjU05LIke3wxS6Lfm4HJoS8EOrv2U=";
          };
          vibratoDictKindaiBungo = buildUnidicVibratoDict {
            name = "unidic-kindai-bungo";
            url = "https://clrd.ninjal.ac.jp/unidic_archive/2512/unidic-kindai-bungo-v202512.zip";
            hash = "sha256-92+UbTLIuatnm65S+528yK+4A3T93ZFlOQ5+1C71D0c=";
          };
          vibratoDictKinseiEdo = buildUnidicVibratoDict {
            name = "unidic-kinsei-edo";
            url = "https://clrd.ninjal.ac.jp/unidic_archive/2512/unidic-kinsei-edo-v202512.zip";
            hash = "sha256-5mrD9DjCadBDzC01zPpY9dkz1xOQkJsiUZCBmEaYvdQ=";
          };
          vibratoDictKinseiBungo = buildUnidicVibratoDict {
            name = "unidic-kinsei-bungo";
            url = "https://clrd.ninjal.ac.jp/unidic_archive/2512/unidic-kinsei-bungo-v202512.zip";
            hash = "sha256-0kw9488O5+61/hI5DIPmmv+tL960e1vrs+20XT8oFjU=";
          };

          vibratoDictionaries = pkgs.symlinkJoin {
            name = "vibrato-dictionaries";
            paths = [
              vibratoDictCwj
              vibratoDictCsj
              vibratoDictNovel
              vibratoDictQkana
              vibratoDictKindaiBungo
              vibratoDictKinseiEdo
              vibratoDictKinseiBungo
            ];
          };

          sudachiRs = pkgs.runCommand "sudachi-rs-f4dd8f20-resources" { } ''
            mkdir -p "$out/share/sudachi/resources"
            cp -R ${inputs.sudachi-rs-src}/resources/. "$out/share/sudachi/resources/"
            test -s "$out/share/sudachi/resources/char.def"
          '';

          buildSudachiDictionary =
            {
              edition,
              hash,
            }:
            let
              archive = pkgs.fetchurl {
                url = "https://d2ej7fkh96fzlu.cloudfront.net/sudachidict/sudachi-dictionary-20260723-${edition}.zip";
                inherit hash;
              };
            in
            pkgs.runCommand "sudachidict-${edition}-20260723"
              {
                nativeBuildInputs = [ pkgs.unzip ];
              }
              ''
                unpacked="$(mktemp -d)"
                unzip -q ${archive} -d "$unpacked"
                dictionary="$(find "$unpacked" -type f -name 'system_${edition}.dic' -print -quit)"
                test -n "$dictionary"
                mkdir -p "$out/share/sudachi" "$out/share/licenses/sudachidict-${edition}"
                cp "$dictionary" "$out/share/sudachi/system_${edition}.dic"
                license="$(find "$unpacked" -type f \( -iname 'LICENSE*' -o -iname 'COPYING*' \) -print -quit)"
                test -n "$license"
                cp "$license" "$out/share/licenses/sudachidict-${edition}/"
                test -s "$out/share/sudachi/system_${edition}.dic"
              '';

          sudachiDictSmall = buildSudachiDictionary {
            edition = "small";
            hash = "sha256-RynRNP0LmFLazInFkilmT+5AZCu1K6/PTvtDLO6bsiw=";
          };
          sudachiDictCore = buildSudachiDictionary {
            edition = "core";
            hash = "sha256-tug19jRA+XR0wtpF2AlQ9zdG5jLkC7/BaLQEFykTXh8=";
          };
          sudachiDictFull = buildSudachiDictionary {
            edition = "full";
            hash = "sha256-/IdSWkx2OepG2Bo+Tjl2hTJA7Afj2zmRGQySCgLv4Qc=";
          };
          sudachiDictionaries = pkgs.symlinkJoin {
            name = "sudachidictionaries-20260723";
            paths = [
              sudachiDictSmall
              sudachiDictCore
              sudachiDictFull
            ];
          };
          sudachiPackagesSmoke = pkgs.runCommand "sudachi-packages-smoke" { } ''
            test -s ${sudachiRs}/share/sudachi/resources/char.def
            test -s ${sudachiDictSmall}/share/sudachi/system_small.dic
            test -s ${sudachiDictCore}/share/sudachi/system_core.dic
            test -s ${sudachiDictFull}/share/sudachi/system_full.dic
            test -s ${sudachiDictionaries}/share/sudachi/system_small.dic
            test -s ${sudachiDictionaries}/share/sudachi/system_core.dic
            test -s ${sudachiDictionaries}/share/sudachi/system_full.dic
            ! grep -q 'sudachi-dictionary-latest' ${./flake.nix}
            touch "$out"
          '';

          stanzaResources = pkgs.fetchurl {
            url = "https://raw.githubusercontent.com/stanfordnlp/stanza-resources/9e22b9943b68a0c83bf9187131992825c1a3c829/resources_1.14.0.json";
            hash = "sha256-TkHB3xUhRvom7QwAagj+6npguzQUu21X29okrS48uZw=";
          };
          stanzaFrGsdTokenize = pkgs.fetchurl {
            url = "https://huggingface.co/stanfordnlp/stanza-fr/resolve/v1.14.0/models/tokenize/gsd.pt";
            hash = "sha256-XZEEdS8AkkYyS3dYHGBfvuuGGgsIV1YqI/CykbZKY3s=";
          };
          stanzaFrGsdMwt = pkgs.fetchurl {
            url = "https://huggingface.co/stanfordnlp/stanza-fr/resolve/v1.14.0/models/mwt/gsd.pt";
            hash = "sha256-+ElJ0sDqKomuY5xlD+vxItOLEWnyg6oZLPKTW2w9/QY=";
          };
          stanzaFrGsdPos = pkgs.fetchurl {
            url = "https://huggingface.co/stanfordnlp/stanza-fr/resolve/v1.14.0/models/pos/gsd_nocharlm.pt";
            hash = "sha256-BcFb1lk/fWTQrf8VXillZp8x/Uv3791IEl4D5Xp36Ak=";
          };
          stanzaFrGsdLemma = pkgs.fetchurl {
            url = "https://huggingface.co/stanfordnlp/stanza-fr/resolve/v1.14.0/models/lemma/gsd_nocharlm.pt";
            hash = "sha256-wNTiUjmc8n5GJx1OBudFVnkAp4V/Qb5Vqx1FH3Ct910=";
          };
          stanzaFrConll17Pretrain = pkgs.fetchurl {
            url = "https://huggingface.co/stanfordnlp/stanza-fr/resolve/v1.14.0/models/pretrain/conll17.pt";
            hash = "sha256-yMRWwaSPujLUq4J659P+xARX9Y6tfhl3tiZ6/DdHSjw=";
          };
          stanzaProfilePython = pkgs.python314.withPackages (pythonPackages: [
            pythonPackages.blake3
          ]);
          stanzaModelFrGsdMorphology =
            pkgs.runCommand "stanza-model-fr-gsd-morphology-1.14.0"
              {
                nativeBuildInputs = [ stanzaProfilePython ];
              }
              ''
                model="$out/share/stanza/fr-gsd-morphology"
                mkdir -p "$model/fr"/{tokenize,mwt,pos,lemma,pretrain}
                cp ${stanzaResources} "$model/resources.json"
                cp ${stanzaFrGsdTokenize} "$model/fr/tokenize/gsd.pt"
                cp ${stanzaFrGsdMwt} "$model/fr/mwt/gsd.pt"
                cp ${stanzaFrGsdPos} "$model/fr/pos/gsd_nocharlm.pt"
                cp ${stanzaFrGsdLemma} "$model/fr/lemma/gsd_nocharlm.pt"
                cp ${stanzaFrConll17Pretrain} "$model/fr/pretrain/conll17.pt"
                PYTHONPATH=${./python/stanza-adapter/src} python - "$model" <<'PY'
                import sys
                from pathlib import Path
                from lingrep_stanza_adapter.bundle import canonical_json, declared_model_profile

                profile = declared_model_profile(
                    language="fr",
                    packages={
                        "tokenize": "gsd",
                        "mwt": "gsd",
                        "pos": "gsd_nocharlm",
                        "lemma": "gsd_nocharlm",
                    },
                )
                (Path(sys.argv[1]) / "lingrep-stanza-model.json").write_bytes(
                    canonical_json(profile) + b"\n"
                )
                PY
              '';
          stanzaModelSmoke = pkgs.runCommand "stanza-model-fr-gsd-morphology-smoke" { } ''
            model=${stanzaModelFrGsdMorphology}/share/stanza/fr-gsd-morphology
            test -s "$model/resources.json"
            test -s "$model/lingrep-stanza-model.json"
            test -s "$model/fr/tokenize/gsd.pt"
            test -s "$model/fr/mwt/gsd.pt"
            test -s "$model/fr/pos/gsd_nocharlm.pt"
            test -s "$model/fr/lemma/gsd_nocharlm.pt"
            test -s "$model/fr/pretrain/conll17.pt"
            touch "$out"
          '';

          linguisticEvalImages = import ./nix/linguistic-eval {
            inherit pkgs;
            lingrep = self'.packages.lingrep;
            vibratoDictionary = vibratoDictCwj;
          };

          lingrepApp = {
            type = "app";
            program = lib.getExe self'.packages.lingrep;
          };
          lingrepWebApp = {
            type = "app";
            program = lib.getExe self'.packages.lingrep-web;
          };
          spacyAdapterCheck = pkgs.writeShellApplication {
            name = "check-spacy-adapter";
            runtimeInputs = [
              pkgs.python314
              pkgs.uv
            ];
            text = ''
              export LINGREP_REPO_ROOT=${./.}
              exec ${pkgs.bash}/bin/bash ${./scripts/check-spacy-adapter.sh} static
            '';
          };
          stanzaAdapterCheck = pkgs.writeShellApplication {
            name = "check-stanza-adapter";
            runtimeInputs = [
              pkgs.python314
              pkgs.uv
            ];
            text = ''
              export LINGREP_REPO_ROOT=${./.}
              exec ${pkgs.bash}/bin/bash ${./scripts/check-stanza-adapter.sh} static
            '';
          };
        in
        {
          _module.args.pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ (import inputs.rust-overlay) ];
          };

          apps.default = lingrepApp;
          apps.lingrep = lingrepApp;
          apps.ling = lingrepApp;
          apps.lingrep-web = lingrepWebApp;
          apps.check-spacy-adapter = {
            type = "app";
            program = lib.getExe spacyAdapterCheck;
          };
          apps.check-stanza-adapter = {
            type = "app";
            program = lib.getExe stanzaAdapterCheck;
          };

          formatter = pkgs.nixfmt;

          # One merged definition rather than scattered `packages.<name> =`
          # bindings, because the Linux-only entries at the end have to be
          # *absent* on darwin: `lib.mkIf` leaves a flake-parts option declared
          # but undefined, which errors on access instead of dropping the
          # attribute.
          packages = {
            default = self'.packages.lingrep;

            # The public package deliberately installs the one supported CLI.
            # Other workspace binaries remain development targets, not part of
            # the published command surface.
            lingrep =
              (mkLingrepPackage {
                cargoExtraArgs = "--package lingrep";
                postInstall = ''
                  mkdir -p \
                    "$out/share/bash-completion/completions" \
                    "$out/share/zsh/site-functions" \
                    "$out/share/fish/vendor_completions.d"
                  "$out/bin/ling" completions bash > "$out/share/bash-completion/completions/ling"
                  "$out/bin/ling" completions zsh > "$out/share/zsh/site-functions/_ling"
                  "$out/bin/ling" completions fish > "$out/share/fish/vendor_completions.d/ling.fish"
                  test -s "$out/share/bash-completion/completions/ling"
                  test -s "$out/share/zsh/site-functions/_ling"
                  test -s "$out/share/fish/vendor_completions.d/ling.fish"
                  man_stage="$(mktemp -d)/pages"
                  "$out/bin/ling" manpages --output "$man_stage" > /dev/null
                  mkdir -p "$out/share/man/man1"
                  cp "$man_stage"/*.1 "$out/share/man/man1/"
                  test -s "$out/share/man/man1/ling.1"
                  test -s "$out/share/man/man1/ling-matches.1"
                  test -s "$out/share/man/man1/ling-corpus-build.1"
                '';
              }).overrideAttrs
                (old: {
                  meta = (old.meta or { }) // {
                    mainProgram = "ling";
                  };
                });

            # lingrep-web has neither shell completions nor manpages, so the package
            # body is just the binary built with the same source filter that
            # keeps the .sqlx cache and web assets hermetic.
            lingrep-web =
              (mkLingrepPackage {
                cargoExtraArgs = "--package lingrep-web";
              }).overrideAttrs
                (old: {
                  meta = (old.meta or { }) // {
                    mainProgram = "lingrep-web";
                  };
                });

            mecab-dic-converter = mecabDicConverter;
            vibrato-dict-cwj = vibratoDictCwj;
            vibrato-dict-csj = vibratoDictCsj;
            vibrato-dict-novel = vibratoDictNovel;
            vibrato-dict-qkana = vibratoDictQkana;
            vibrato-dict-kindai-bungo = vibratoDictKindaiBungo;
            vibrato-dict-kinsei-edo = vibratoDictKinseiEdo;
            vibrato-dict-kinsei-bungo = vibratoDictKinseiBungo;
            vibrato-dictionaries = vibratoDictionaries;
            sudachi-rs = sudachiRs;
            sudachidict-small = sudachiDictSmall;
            sudachidict-core = sudachiDictCore;
            sudachidict-full = sudachiDictFull;
            sudachidictionaries = sudachiDictionaries;
            stanza-model-fr-gsd-morphology = stanzaModelFrGsdMorphology;
          }
          # The OCI images stuff host `pkgs` into a Linux image, so on darwin
          # they would package Mach-O binaries rather than cross-compile.
          // lib.optionalAttrs isLinux {
            linguistic-eval-lingrep-image = linguisticEvalImages.lingrep;
            linguistic-eval-mecab-image = linguisticEvalImages.mecab;
          };

          devShells.default = self'.devShells.nightly;

          # What CI enters. See `ciDevDeps` for what it leaves out and why. The
          # toolchain is the default shell's, not stable: `./ci.sh --fast` runs
          # `cargo config get`, which cargo still gates behind nightly, so the
          # lane a developer runs through `.githooks/pre-push` and the lane CI
          # runs are the same lane on the same toolchain.
          devShells.ci = mkDevShellWith {
            rustToolchain = pkgs.rust-bin.nightly.latest.default;
            packages = ciDevDeps;
            profilingExtras = false;
          };

          devShells.nightly = mkDevShell (
            pkgs.rust-bin.nightly.latest.default.override { extensions = [ "rust-src" ]; }
          );
          devShells.stable = mkDevShell (
            pkgs.rust-bin.stable.latest.default.override { extensions = [ "rust-src" ]; }
          );
          devShells.msrv = mkDevShell (
            pkgs.rust-bin.stable.${msrv}.default.override { extensions = [ "rust-src" ]; }
          );

          # Merged for the same reason `packages` is: the two Linux-only checks
          # must be absent on darwin, not present-and-undefined.
          checks = {
            clippy = craneLib.cargoClippy (
              commonArgs
              // {
                inherit cargoArtifacts;
                # `hegeltest-c` invokes Cargo metadata from its build script and cannot
                # resolve its nested dependency graph from Crane's vendor directory.
                # CI retains `cargo clippy --all-targets`; this flake check covers the
                # production library and binaries in the sandbox.
                cargoClippyExtraArgs = "--lib --bins";
                doCheck = false;
              }
            );

            # MSRV gate: builds the whole workspace on the pinned 1.95.0 toolchain,
            # so any crate that stops compiling on the declared MSRV fails CI. The toolchain is
            # overridden for the dependency build too, or this would silently test
            # the default toolchain instead of 1.95.0.
            msrv-build =
              let
                msrvCrane = craneLib.overrideToolchain pkgs.rust-bin.stable.${msrv}.default;
              in
              msrvCrane.buildPackage (
                commonArgs
                // {
                  pname = "lingrep-msrv";
                  cargoArtifacts = msrvCrane.buildDepsOnly commonArgs;
                  doCheck = false;
                }
              );

            mecab-dic-converter = mecabDicConverter;
            sudachi-packages = sudachiPackagesSmoke;
            stanza-model-fr-gsd-morphology = stanzaModelSmoke;
          }
          // lib.optionalAttrs isLinux {
            # The standard-shell smoke needs bubblewrap, which is Linux-only
            # by construction (user namespaces).
            linguistic-eval-standard-shell = linguisticEvalImages.shellSmoke;

            # Smoke test for the NixOS module. Buildable in any sandbox; boot
            # needs KVM. CI runs `nix flake check --no-build`, which reaches
            # the driver build but skips the VM boot; on a machine with KVM
            # the test runs end-to-end via `nix build .#checks.x86_64-linux
            # .lingrep-web-nixos-smoke.driver && ./result/bin/nixos-test-driver`.
            #
            # `nixosTest` does not throw on darwin — it evaluates and then
            # fails deep inside qemu — and the `meta.platforms` at
            # test.nix:22 is documentation that excludes nothing, so dropping
            # the attribute is the only thing that actually gates it.
            lingrep-web-nixos-smoke = import ./nix/modules/lingrep-web/test.nix {
              inherit pkgs;
              package = self'.packages.lingrep-web;
            };

            # The corpus link naming rule, by evaluation rather than by boot,
            # so `nix flake check --no-build` reaches it where it reaches only
            # the smoke test's driver.
            lingrep-web-corpus-link-naming = import ./nix/modules/lingrep-web/link-naming-test.nix {
              inherit pkgs;
              package = self'.packages.lingrep-web;
            };
          };
        };
    };
}
