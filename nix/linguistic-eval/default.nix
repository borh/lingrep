{
  pkgs,
  lingrep,
  vibratoDictionary,
}:
let
  unidicCwj = pkgs.stdenv.mkDerivation {
    pname = "unidic-cwj";
    version = "202512";
    src = pkgs.fetchzip {
      url = "https://clrd.ninjal.ac.jp/unidic_archive/2512/unidic-cwj-202512.zip";
      hash = "sha256-lNvcTlhqSWoLPF104eS7r08Dw5SQYRwzTQXy6DxdfjM=";
      stripRoot = false;
    };
    phases = [
      "unpackPhase"
      "installPhase"
    ];
    installPhase = ''
      install -d "$out/share/mecab/dic/unidic-cwj"
      install -m 644 dicrc *.def *.bin *.dic "$out/share/mecab/dic/unidic-cwj"
    '';
  };

  # Adapted from ../nix/pkgs/mecab. The benchmark deliberately omits that
  # package's -march=native flags so the OCI artifact is host-portable.
  mecab = pkgs.stdenv.mkDerivation rec {
    pname = "mecab";
    version = "0.996.13";
    src = pkgs.fetchFromGitHub {
      owner = "shogo82148";
      repo = pname;
      rev = "v${version}";
      hash = "sha256-jfP4i23q5Y8kmsrnf+ToLLv4eqPkkWqLyJLbrh19amw=";
    };
    sourceRoot = "${src.name}/mecab";
    nativeBuildInputs = [ pkgs.autoreconfHook ];
    buildInputs = [
      pkgs.gettext
      pkgs.libiconv
    ];
    patches = [ ./gettext.patch ];
    postPatch = ''
      for source in src/*.h src/*.cpp; do
        substituteInPlace "$source" --replace-quiet "register " ""
      done
    '';
    configureFlags = [
      "--with-charset=utf-8"
      "--enable-utf8-only"
    ];
    makeFlags = [ "CXXFLAGS=-std=c++14" ];
    postInstall = ''
      install -Dm644 mecabrc "$out/etc/mecabrc"
      sed -i \
        's|^dicdir = .*$|dicdir = ${unidicCwj}/share/mecab/dic/unidic-cwj|' \
        "$out/etc/mecabrc"
    '';
    doCheck = true;
  };

  commonContents = with pkgs; [
    asciinema
    bashInteractive
    binutils-unwrapped
    bubblewrap
    cacert
    coreutils
    diffutils
    file
    findutils
    gawk
    gnugrep
    gnused
    jq
    less
    ncurses
    procps
    python3
    tmux
    util-linux
    which
  ];

  shellSmoke =
    pkgs.runCommand "linguistic-eval-standard-shell-smoke"
      {
        nativeBuildInputs = commonContents;
      }
      ''
        for command in \
          awk bash cat clear cmp diff file find grep jq less ps python3 \
          reset sed strings tmux tput which
        do
          command -v "$command" >/dev/null
        done
        touch "$out"
      '';

  toolLauncher =
    pkgs.runCommand "lingrep-harbor-tool-launcher"
      {
        nativeBuildInputs = [
          pkgs.rustc
          pkgs.stdenv.cc
        ];
      }
      ''
        mkdir -p "$out/bin" build
        rustc --edition=2021 --test \
          ${../../benchmarks/harbor/tool_launcher.rs} \
          -o build/tool-launcher-tests
        build/tool-launcher-tests
        rustc --edition=2021 -C opt-level=2 -C strip=symbols \
          ${../../benchmarks/harbor/tool_launcher.rs} \
          -o "$out/bin/lingrep-harbor-tool-launcher"
      '';

  mkToolWrapper =
    {
      toolName,
      realTool,
    }:
    pkgs.writeShellScriptBin toolName ''
      exec ${toolLauncher}/bin/lingrep-harbor-tool-launcher \
        --log-root /logs/agent/tool-invocations \
        --tool ${pkgs.lib.escapeShellArg toolName} \
        --real ${pkgs.lib.escapeShellArg realTool} \
        -- "$@"
    '';

  mkImage =
    {
      name,
      toolName,
      realTool,
      contents,
      environment,
      verifierTool ? null,
    }:
    let
      toolWrapper = mkToolWrapper { inherit toolName realTool; };
    in
    pkgs.dockerTools.buildLayeredImage {
      inherit name;
      contents = contents ++ [ toolWrapper ];
      tag = "latest";
      extraCommands = ''
        mkdir -p app data home/agent logs/artifacts tmp usr/bin usr/local/bin
        ln -s /bin/env usr/bin/env
        ln -s /bin/jq usr/bin/jq
        ln -s /bin/python3 usr/bin/python3
        ln -s ${toolWrapper}/bin/${toolName} usr/local/bin/${toolName}
        ${pkgs.lib.optionalString (verifierTool != null) ''
          ln -s ${verifierTool} usr/local/bin/ling-verifier
        ''}
        chmod 1777 tmp logs/artifacts
      '';
      config = {
        Cmd = [ "/bin/bash" ];
        Env = [
          "HOME=/home/agent"
          "LANG=C.UTF-8"
          "LC_ALL=C.UTF-8"
          "PATH=/usr/local/bin:/usr/bin:/bin"
        ]
        ++ environment;
        WorkingDir = "/app";
      };
    };
in
{
  inherit shellSmoke;
  lingrep = mkImage {
    name = "lingrep-linguistic-eval";
    toolName = "ling";
    realTool = "${lingrep}/bin/ling";
    contents = commonContents ++ [
      vibratoDictionary
    ];
    environment = [
      "LINGREP_DICTIONARY_PATH=${vibratoDictionary}/share/vibrato/unidic-cwj-202512.dic"
    ];
    verifierTool = "${lingrep}/bin/ling";
  };
  mecab = mkImage {
    name = "mecab-linguistic-eval";
    toolName = "mecab";
    realTool = "${mecab}/bin/mecab";
    contents = commonContents ++ [
      unidicCwj
    ];
    environment = [ ];
  };
}
