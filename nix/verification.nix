{ pkgs, buildDeps }:
let
  kaniVersion = "0.68.0";
  kaniToolchain = pkgs.rust-bin.nightly."2026-08-21".minimal;
  kani = pkgs.stdenvNoCC.mkDerivation {
    pname = "kani-verifier";
    version = kaniVersion;
    src = pkgs.fetchurl {
      url = "https://github.com/model-checking/kani/releases/download/kani-${kaniVersion}/kani-${kaniVersion}-x86_64-unknown-linux-gnu.tar.gz";
      hash = "sha256-MuK0hNc+3gu/ZKLPCHnEJZQiSX2K7E7mfUSN6a54Q9M=";
    };
    nativeBuildInputs = [
      pkgs.autoPatchelfHook
      pkgs.makeWrapper
    ];
    buildInputs = [ pkgs.stdenv.cc.cc.lib ];
    dontBuild = true;
    dontStrip = true;
    installPhase = ''
      runHook preInstall
      mkdir -p "$out/libexec/kani" "$out/bin"
      cp -R . "$out/libexec/kani"
      ln -s ${kaniToolchain} "$out/libexec/kani/toolchain"
      # Kani 0.68.0 injects a macro_use extern crate during proof compilation.
      makeWrapper "$out/libexec/kani/bin/kani-driver" "$out/bin/kani" \
        --prefix PATH : "$out/libexec/kani/bin" \
        --prefix RUSTFLAGS ' ' --force-warn=macro_use_extern_crate
      ln -s kani "$out/bin/cargo-kani"
      runHook postInstall
    '';
    preFixup = ''
      addAutoPatchelfSearchPath ${kaniToolchain}/lib
    '';
  };

  verusVersion = "0.2026.11.02.8bf68ee";
  verusToolchain = pkgs.rust-bin.stable."1.99.1".minimal;
  verus = pkgs.stdenvNoCC.mkDerivation {
    pname = "verus";
    version = verusVersion;
    src = pkgs.fetchurl {
      url = "https://github.com/verus-lang/verus/releases/download/release/${verusVersion}/verus-${verusVersion}-x86-linux.zip";
      hash = "sha256-86ARkpLfiFXkQ12oEwvuGHOw5qYMMGrM+BoI6mh7ujw=";
    };
    nativeBuildInputs = [
      pkgs.autoPatchelfHook
      pkgs.makeWrapper
      pkgs.unzip
    ];
    buildInputs = [ pkgs.stdenv.cc.cc.lib ];
    dontBuild = true;
    dontStrip = true;
    installPhase = ''
      runHook preInstall
      mkdir -p "$out/libexec/verus" "$out/bin"
      cp -R . "$out/libexec/verus"
      makeWrapper "$out/libexec/verus/verus" "$out/bin/verus" \
        --set VERUS_USE_RUSTUP 0 \
        --prefix PATH : ${verusToolchain}/bin
      runHook postInstall
    '';
    preFixup = ''
      addAutoPatchelfSearchPath ${verusToolchain}/lib
    '';
  };
in
pkgs.mkShell {
  packages = buildDeps ++ [
    kaniToolchain
    kani
    verus
  ];
  RUSTFLAGS = "";
  RUSTC_WRAPPER = "";
}
