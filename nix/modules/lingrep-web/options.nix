# Option schema for the lingrep-web NixOS module.
#
# An operator declares:
#   * one dictionary path (a Vibrato `*.dic.zst` or `*.dic`);
#   * an attrset of corpora keyed by name; the optional entry named
#     `default` becomes the `--input` corpus and the others become
#     repeated `--corpus NAME=PATH` (a server with only named corpora is
#     valid, because `--input` is optional in the binary). Each entry is
#     either a corpus *source* or a prepared `.lingcorpus` artifact, which
#     is how a format lingrep-web has no reader for reaches the browser;
#   * a list of pattern catalogs (each a directory containing `*.lx` files);
#   * host/port/firewall settings;
#   * logging environment.
#
# The activator lives in `nixos.nix`; this file is the option shape alone.

{ lib, ... }:

{
  options.services.lingrep-web = {
    enable = lib.mkEnableOption "lingrep-web pattern-matching web interface";

    package = lib.mkOption {
      type = lib.types.package;
      description = ''
        Package providing the `lingrep-web` binary. The flake's
        `packages.<system>.lingrep-web` is the supported choice; pass it as
        `inputs.lingrep.packages.<nix system>.lingrep-web` (where the
        system string comes from `pkgs.stdenv.hostPlatform.system`) in
        `configuration.nix`.
      '';
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "lingrep-web";
      description = "System user the lingrep-web process runs as.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "lingrep-web";
      description = "System group the lingrep-web process runs as.";
    };

    dictionary = lib.mkOption {
      type = lib.types.path;
      description = ''
        Path to the Vibrato dictionary (`*.dic.zst` or `*.dic`). The flake's
        `vibrato-dict-cwj` (and other `vibrato-dict-*` outputs) point at
        suitable files; pass the result of `nix build .#vibrato-dict-cwj
        --no-link --print-out-paths` joined with
        `/share/vibrato/unidic-cwj-202512.dic.zst`.
      '';
    };

    corpora = lib.mkOption {
      type = lib.types.attrsOf (
        lib.types.submodule {
          options = {
            path = lib.mkOption {
              type = lib.types.path;
              description = ''
                Filesystem path to the corpus. Either a **source** — a file
                or a directory lingrep-web reads and prepares itself — or a
                **prepared V8 artifact**, a directory carrying a
                `manifest.json` (`lingrep_corpus::is_prepared_artifact` tells the
                two apart structurally, so nothing declares which this is).

                A prepared artifact is producer-free: it records the reader
                that built it, is read in place, and is never written to or
                deleted. That is the supported route for a format lingrep-web has
                no reader for — notably BCCWJ, whose C-XML reader lives in
                `ling`. Passing a BCCWJ *distribution root* here is
                refused at startup; prepare it first with
                `ling corpus build --input-format bccwj <root>` and
                declare the resulting `*.lingcorpus` directory.

                Write this as a Nix *string*, not a path literal: an
                unquoted `/data/…` would copy the corpus into the store.
              '';
            };
            format = lib.mkOption {
              type = lib.types.nullOr (
                lib.types.enum [
                  "plain"
                  "jsonl"
                  "markdown"
                ]
              );
              default = null;
              description = ''
                Override the inferred corpus format. Only meaningful on the
                `default` corpus: lingrep-web forwards it as `--input-format`, a
                single global override, and refuses it when more than one
                corpus is declared. Setting `format` on a non-default entry
                is a module-level error — drop the field there and let
                lingrep-web infer per path.

                There is deliberately no `bccwj` value: `--input-format
                bccwj` is a ling format that lingrep-web refuses. Serve a
                BCCWJ corpus as a prepared artifact instead (see `path`).

                Reader configuration reaches artifact identity, so lingrep-web
                *refuses* `--input-format` for a prepared artifact rather
                than ignoring it — a unit that passed both would fail to
                start. The assertion in `nixos.nix` catches the case it can
                see at eval time, a `path` ending in `.lingcorpus`.
              '';
            };
          };
        }
      );
      description = ''
        Declared corpora, keyed by `[A-Za-z0-9_-]{1,64}` name. The optional
        entry named `default` becomes the `--input` corpus and the others
        become repeated `--corpus NAME=PATH`; at least one entry is
        required, but a server with only named corpora is valid because
        `--input` is optional in lingrep-web. The reserved `default` key is an
        operator-side convention; lingrep-web rejects `--corpus default=PATH`
        while still serving `k=default` in permalinks.
      '';
      example = lib.literalExpression ''
        {
          default = { path = "/var/lib/lingrep-web/corpora/main.jsonl"; };
          sci = { path = "/var/lib/lingrep-web/corpora/science/"; format = "markdown"; };
        }
      '';
    };

    patterns = {
      catalogs = lib.mkOption {
        type = lib.types.listOf (lib.types.either lib.types.path lib.types.package);
        default = [ ];
        example = lib.literalExpression ''
          [ (inputs.lingrep.outPath + "/patterns") ]
        '';
        description = ''
          Pattern catalogs. Each entry must be a directory containing
          `*.lx` files (recursively). The module symlinks them under
          `<stateDirectory>/patterns/<name>/` and passes that root as
          `--pattern-root`, so the catalog list is also the discoverable
          catalog list inside the UI.
        '';
      };

      editorSeed = lib.mkOption {
        type = lib.types.str;
        default = "";
        example = "patterns/patterns/sentence-final.lx";
        description = ''
          Path to the editor seed file, forwarded verbatim as `--patterns`
          and so resolved against the unit's **working directory**
          (`/var/lib/lingrep-web`) — not against `--pattern-root`.

          The assembled root is `<state>/patterns/`, and the activation
          script puts each catalog under its own basename inside it, so a
          catalog whose directory is itself named `patterns` is reached at
          `patterns/patterns/<file>.lx`. The doubled component is real.

          Empty (the default) means "no `--patterns` argument"; lingrep-web then
          uses its built-in default (`patterns/sentence-final.lx`, also
          working-directory-relative), which resolves inside the assembled
          root only by coincidence of naming and otherwise produces an
          "empty editor" warning at startup. Set this explicitly.
        '';
      };
    };

    markdown = {
      includeSections = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        description =
          ""
          + "Comma list of section labels to admit (`--md-include-sections`). "
          + "Empty (the default) omits the flag, which admits every label; "
          + "lingrep-web refuses the flag itself in its empty spelling.";
      };
      excludeSections = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ "references" ];
        description = "Comma list of section labels to reject (`--md-exclude-sections`).";
      };
      selfLabels = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ "title" ];
        description = "Heading classes that label only their own heading (`--md-self-labels`).";
      };
    };

    settings = {
      host = lib.mkOption {
        type = lib.types.str;
        default = "127.0.0.1";
        description = ''
          Address to bind. Default loopback. Changing this is opt-in: lingrep-web
          binds whatever the operator asks, but the recommended deployment is
          "loopback + reverse proxy" so TLS and access control stay outside
          lingrep-web.
        '';
      };
      port = lib.mkOption {
        type = lib.types.port;
        default = 3000;
        description = "TCP port to bind (`--port`).";
      };
      basePath = lib.mkOption {
        type = lib.types.str;
        default = "/";
        example = "/research/lingrep-web";
        description = ''
          Absolute path prefix at which lingrep-web serves its routes
          (`--base-path`). The root default serves directly at `/`; a nested
          prefix lets a reverse proxy preserve a public subpath. lingrep-web
          validates that the value is an unambiguous path.
        '';
      };
      openFirewall = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Open `settings.port` in the system firewall. Off by default because
          the recommended deployment is "loopback + reverse proxy on the same
          host"; enabling this is appropriate when the operator runs lingrep-web
          directly on a routable interface.
        '';
      };
      maxKwicLinesPerPattern = lib.mkOption {
        type = lib.types.ints.positive;
        default = 1000;
        description = "Maximum KWIC lines retained per pattern (`--max-kwic-lines-per-pattern`).";
      };
      maxActiveSessions = lib.mkOption {
        type = lib.types.ints.positive;
        default = 256;
        description = "Maximum concurrently retained workbench sessions (`--max-active-sessions`).";
      };
      maxRecordBytes = lib.mkOption {
        type = lib.types.ints.positive;
        default = 1024 * 1024;
        defaultText = lib.literalExpression "1024 * 1024";
        description = ''
          Maximum bytes accepted for one physical input record
          (`--max-record-bytes`). The default mirrors
          `lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES` (1 MiB); raise
          it when reading Markdown or JSONL documents that are known to
          exceed the default.
        '';
      };
    };

    environment = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = {
        RUST_LOG = "info";
      };
      description = "Extra environment variables for the lingrep-web process.";
    };
  };
}
