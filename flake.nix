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
      url = "github:WorksApplications/sudachi.rs/5de0410b6edfd428fc510cc9763440772e9619d5";
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
            # Compose V2 provider for `podman compose`; Harbor's Podman
            # environment preflight runs `podman compose ls`, which
            # podman-compose does not implement.
            docker-compose
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
                isDictionaryLicense = lib.hasPrefix "${toString ./nix/dictionary-licenses}/" (toString path);
              in
              isHtml
              || isSgx
              || isNdc9Data
              || isWebAsset
              || isSqlxCache
              || isDictionaryLicense
              || craneLib.filterCargoSources path type;
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
            version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
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

          # `rust-toolchain.toml` is the one toolchain for development, CI and
          # release builds; rustup reads the same file outside Nix.
          pinnedToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

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
            patches = [ ./nix/patches/mecab-dic-converter-vibrato-rkyv-0.7.18.patch ];
            postPatch = ''
              cp ${./nix/mecab-dic-converter/Cargo.lock} Cargo.lock
            '';
            cargoLock = {
              lockFile = ./nix/mecab-dic-converter/Cargo.lock;
              outputHashes = {
                "vibrato-rkyv-0.7.18" = "sha256-3xirbGNxZBKAaow51pRH1IGpHEvoEQPilsEsFEel/Po=";
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
                passthru.unidicSource = { inherit url hash; };
                nativeBuildInputs = [
                  mecabDicConverter
                  pkgs.zstd
                ];
              }
              ''
                mkdir -p "$out/share/vibrato"
                mkdir -p "$out/share/licenses/unidic-${lib.removePrefix "unidic-" name}-202512"
                cp -R ${./nix/dictionary-licenses + "/${lib.removePrefix "unidic-" name}"}/. \
                  "$out/share/licenses/unidic-${lib.removePrefix "unidic-" name}-202512/"
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
                # dictionary that is built and never loaded.
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

          # UniDic 202512 sources, converted by `buildUnidicVibratoDict`.
          unidicSources = {
            cwj = {
              url = "https://clrd.ninjal.ac.jp/unidic_archive/2512/unidic-cwj-202512.zip";
              hash = "sha256-lNvcTlhqSWoLPF104eS7r08Dw5SQYRwzTQXy6DxdfjM=";
            };
            csj = {
              url = "https://clrd.ninjal.ac.jp/unidic_archive/2512/unidic-csj-202512.zip";
              hash = "sha256-W0toSrgD8R+KLWos1XuaR3qJhiiYLXtwyd1sUvuYFAM=";
            };
            novel = {
              url = "https://clrd.ninjal.ac.jp/unidic_archive/2512/unidic-novel-v202512.zip";
              hash = "sha256-19wqA64F2CJFYaHzZTMCxOcwYoOTd6C98GHa5b/RUH8=";
            };
            qkana = {
              url = "https://clrd.ninjal.ac.jp/unidic_archive/2512/unidic-qkana-v202512.zip";
              hash = "sha256-bzlybu2NBEFCsfTjU05LIke3wxS6Lfm4HJoS8EOrv2U=";
            };
            kindai-bungo = {
              url = "https://clrd.ninjal.ac.jp/unidic_archive/2512/unidic-kindai-bungo-v202512.zip";
              hash = "sha256-92+UbTLIuatnm65S+528yK+4A3T93ZFlOQ5+1C71D0c=";
            };
            kinsei-edo = {
              url = "https://clrd.ninjal.ac.jp/unidic_archive/2512/unidic-kinsei-edo-v202512.zip";
              hash = "sha256-5mrD9DjCadBDzC01zPpY9dkz1xOQkJsiUZCBmEaYvdQ=";
            };
            kinsei-bungo = {
              url = "https://clrd.ninjal.ac.jp/unidic_archive/2512/unidic-kinsei-bungo-v202512.zip";
              hash = "sha256-0kw9488O5+61/hI5DIPmmv+tL960e1vrs+20XT8oFjU=";
            };
          };

          # SHA-256 of each `.dic.zst` attached to the `unidic-202512` release,
          # which holds the zstd-compressed output of `buildUnidicVibratoDict`.
          unidicReleaseHashes = {
            cwj = "sha256-lBmNq0SkSZzWNC9ZyfEVoY1dLCYuFCZbbOhGmHgOL0o=";
            csj = "sha256-gJoQH3Ei+uxQ3hb9uJxLkvAXdc5Tz4NtVBzO45AZSIg=";
            novel = "sha256-q53G/z3iXHuyAA/0mdL73A/S7zwampEj7EK97JhuRPo=";
            qkana = "sha256-6CyHMaWOwCebNgzXu96uCQ+xfJdKd0qMxFT1abNKqMo=";
            kindai-bungo = "sha256-5eZOpmPRo/dffFuCke0yKaclvzVZMJ/r66LUAPQyW4A=";
            kinsei-edo = "sha256-mY7G9V2SPAFwULMW5jn6lG4ecQb0y/AZJGPaVbUNzP4=";
            kinsei-bungo = "sha256-xGNfY02brVkK4a1xot4BSCVjKxylWkR0hZWcO5RBigk=";
          };

          fetchUnidicVibratoDict =
            name: hash:
            pkgs.runCommand "vibrato-dict-unidic-${name}-202512"
              {
                nativeBuildInputs = [ pkgs.zstd ];
                archive = pkgs.fetchurl {
                  url = "https://github.com/borh/lingrep/releases/download/unidic-202512/unidic-${name}-202512.dic.zst";
                  inherit hash;
                };
              }
              ''
                mkdir -p "$out/share/vibrato"
                mkdir -p "$out/share/licenses/unidic-${name}-202512"
                cp -R ${./nix/dictionary-licenses + "/${name}"}/. \
                  "$out/share/licenses/unidic-${name}-202512/"
                zstd -q -d "$archive" -o "$out/share/vibrato/unidic-${name}-202512.dic"
              '';

          vibratoDicts = lib.mapAttrs fetchUnidicVibratoDict unidicReleaseHashes;
          vibratoDictCwj = vibratoDicts.cwj;

          vibratoDictionariesFromSource = pkgs.symlinkJoin {
            name = "vibrato-dictionaries-from-source";
            paths = lib.mapAttrsToList (
              name: source: buildUnidicVibratoDict ({ name = "unidic-${name}"; } // source)
            ) unidicSources;
          };

          vibratoDictionaries = pkgs.symlinkJoin {
            name = "vibrato-dictionaries";
            paths = lib.attrValues vibratoDicts;
            # A dictionary's BLAKE3 is part of every Vibrato analysis identity,
            # so a converter or input change that alters these bytes makes
            # every corpus prepared with that dictionary unreadable. This test
            # checks the released files and a conversion from source against
            # the same digests; run it with
            # `nix build .#vibrato-dictionaries.tests.digests` after a
            # flake.lock update. Replace a digest only together with a new
            # release and a CHANGELOG entry stating which corpora must be rebuilt.
            passthru.tests.digests =
              pkgs.runCommand "vibrato-dictionary-digests"
                {
                  nativeBuildInputs = [ pkgs.b3sum ];
                  digests = ''
                    80aeaf4225676386368932b1f632d71e05d81b4be185e0b786c360656944abbf  unidic-cwj-202512.dic
                    80c6e211be770d4e8ee0e2443584f285818e508acea84f21ea03d5eb47afe37f  unidic-csj-202512.dic
                    20ff3deb8b988e345e78af3079a61a023ceaa974151988046931c94894d66348  unidic-novel-202512.dic
                    c15e76a03d56bceec589da2ac6b93c7491b5f01bf1be8b8b5c1ae1075490f955  unidic-qkana-202512.dic
                    a12b36972086e170e428eef56b53d8eafe8b24d16628c9edf7370a87b804bb8b  unidic-kindai-bungo-202512.dic
                    9aa4a90441d4209b2b59d9eee2609be79f3307df56083c5d618afd00d458604b  unidic-kinsei-edo-202512.dic
                    8fbb017faaff3d2ab412a6d23195b51b5adf412c1742d14c8da039623bb52ea9  unidic-kinsei-bungo-202512.dic
                  '';
                  passAsFile = [ "digests" ];
                }
                ''
                  for dictionaries in ${vibratoDictionaries} ${vibratoDictionariesFromSource}; do
                    (cd "$dictionaries/share/vibrato" && b3sum --check "$digestsPath")
                  done
                  touch "$out"
                '';
          };

          sudachiRs = pkgs.runCommand "sudachi-rs-0.7.0-resources" { } ''
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
                url = "https://github.com/WorksApplications/SudachiDict/releases/download/v20261015/sudachi-dictionary-20261015-${edition}.zip";
                inherit hash;
              };
            in
            pkgs.runCommand "sudachidict-${edition}-20261015"
              {
                nativeBuildInputs = [ pkgs.unzip ];
              }
              ''
                unpacked="$(mktemp -d)"
                unzip -q ${archive} -d "$unpacked"
                dictionary="$(find "$unpacked" -type f -name 'system_${edition}.dic' -print -quit)"
                test -n "$dictionary"
                test "$(head -c 16 "$dictionary")" = SudachiBinaryDic
                mkdir -p "$out/share/sudachi" "$out/share/licenses/sudachidict-${edition}"
                cp "$dictionary" "$out/share/sudachi/system_${edition}.dic"
                license="$(find "$unpacked" -type f \( -iname 'LICENSE*' -o -iname 'COPYING*' \) -print -quit)"
                test -n "$license"
                cp "$license" "$out/share/licenses/sudachidict-${edition}/"
                test -s "$out/share/sudachi/system_${edition}.dic"
              '';

          sudachiDictSmall = buildSudachiDictionary {
            edition = "small";
            hash = "sha256-kRNDc63ZJoAngoG2S9kpUvrypoXI0F235yfQ+f+9CfU=";
          };
          sudachiDictCore = buildSudachiDictionary {
            edition = "core";
            hash = "sha256-XPXLfx6pQ6mI4vtdKFqaizU4hh+u+nomBMLNGMOH9QM=";
          };
          sudachiDictFull = buildSudachiDictionary {
            edition = "full";
            hash = "sha256-s4MYyiakRM1xOlo3ejUzCO84xzPGTMC+kFEiUnUWx5M=";
          };
          sudachiDictionaries = pkgs.symlinkJoin {
            name = "sudachidictionaries-20261015";
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

          spacyEnglishSource = pkgs.fetchurl {
            url = "https://github.com/explosion/spacy-models/releases/download/en_core_web_sm-3.8.0/en_core_web_sm-3.8.0.tar.gz";
            hash = "sha256-FKLzG8R2r4cBmBnqjJlI+r39RzpELt1rHLpivwwsD1U=";
          };
          spacyProfilePython = pkgs.python3.withPackages (ps: [
            ps.spacy
            ps.blake3
          ]);
          spacyModelEnCoreWebSm =
            pkgs.runCommand "spacy-model-en-core-web-sm-3.8.0"
              {
                nativeBuildInputs = [ spacyProfilePython ];
                meta.license = lib.licenses.mit;
              }
              ''
                tar xf ${spacyEnglishSource}
                model="$out/share/spacy/en_core_web_sm"
                mkdir -p "$model"
                cp -r en_core_web_sm-3.8.0/en_core_web_sm/en_core_web_sm-3.8.0/. "$model/"
                cp en_core_web_sm-3.8.0/LICENSE "$model/"
                PYTHONPATH=${./python/spacy-adapter/src}:${./python/adapter-protocol/src} python -m lingrep_spacy_adapter \
                  --model-root "$model" --backend cpu --prepare \
                  --boundary-component parser --probe-text "I was speaking. They listened." \
                  --morphology-key feat.Aspect --morphology-key feat.Case \
                  --morphology-key feat.Degree --morphology-key feat.Mood \
                  --morphology-key feat.Number --morphology-key feat.Person \
                  --morphology-key feat.Poss --morphology-key feat.PronType \
                  --morphology-key feat.PunctType --morphology-key feat.Tense \
                  --morphology-key feat.VerbForm
              '';

          spacyEnglishAnalyzer = pkgs.runCommand "spacy-analyzer-en-core-web-sm" { } ''
            project="$out/lib/lingrep-spacy-adapter"
            mkdir -p "$project" "$out/share/lingrep"
            cp -r ${./python/adapter-protocol} "$out/lib/adapter-protocol"
            cp -r ${./python/spacy-adapter/src} "$project/src"
            cp -r ${./python/spacy-adapter/bin} "$project/bin"
            cp ${./python/spacy-adapter/pyproject.toml} "$project/pyproject.toml"
            cp ${./python/spacy-adapter/uv.lock} "$project/uv.lock"
            cat > "$out/share/lingrep/english.toml" <<EOF
            version = 1
            kind = "spacy"
            python = "${spacyProfilePython}/bin/python"
            entrypoint = "$project/bin/lingrep-spacy-adapter"
            model = "${spacyModelEnCoreWebSm}/share/spacy/en_core_web_sm"
            backend = "cpu"
            EOF
          '';

          spacyEnglishSmoke =
            pkgs.runCommand "spacy-model-en-core-web-sm-smoke"
              {
                nativeBuildInputs = [ spacyProfilePython ];
              }
              ''
                export LINGREP_SPACY_ENGLISH_MODEL=${spacyModelEnCoreWebSm}/share/spacy/en_core_web_sm
                export PYTHONPATH=${./python/spacy-adapter/src}:${./python/adapter-protocol/src}
                export PYTHONDONTWRITEBYTECODE=1
                python -m unittest discover -s ${./python/spacy-adapter/tests} -k packaged_model
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
                PYTHONPATH=${./python/stanza-adapter/src}:${./python/adapter-protocol/src} python - "$model" <<'PY'
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
            vibratoDictionary = buildUnidicVibratoDict {
              name = "unidic-cwj";
              inherit (unidicSources.cwj) url hash;
            };
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
            vibrato-dict-cwj = vibratoDicts.cwj;
            vibrato-dict-csj = vibratoDicts.csj;
            vibrato-dict-novel = vibratoDicts.novel;
            vibrato-dict-qkana = vibratoDicts.qkana;
            vibrato-dict-kindai-bungo = vibratoDicts.kindai-bungo;
            vibrato-dict-kinsei-edo = vibratoDicts.kinsei-edo;
            vibrato-dict-kinsei-bungo = vibratoDicts.kinsei-bungo;
            vibrato-dictionaries = vibratoDictionaries;
            vibrato-dictionaries-from-source = vibratoDictionariesFromSource;
            sudachi-rs = sudachiRs;
            sudachidict-small = sudachiDictSmall;
            sudachidict-core = sudachiDictCore;
            sudachidict-full = sudachiDictFull;
            sudachidictionaries = sudachiDictionaries;
            spacy-analyzer-en-core-web-sm = spacyEnglishAnalyzer;
            spacy-model-en-core-web-sm = spacyModelEnCoreWebSm;
            stanza-model-fr-gsd-morphology = stanzaModelFrGsdMorphology;
          }
          # The OCI images stuff host `pkgs` into a Linux image, so on darwin
          # they would package Mach-O binaries rather than cross-compile.
          // lib.optionalAttrs isLinux {
            linguistic-eval-lingrep-image = linguisticEvalImages.lingrep;
            linguistic-eval-mecab-image = linguisticEvalImages.mecab;
          };

          devShells.default = mkDevShell pinnedToolchain;

          # What CI enters. See `ciDevDeps` for what it leaves out and why.
          devShells.ci = mkDevShellWith {
            rustToolchain = pinnedToolchain;
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
            spacy-model-en-core-web-sm = spacyEnglishSmoke;
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
