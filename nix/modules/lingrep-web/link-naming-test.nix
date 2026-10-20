# The corpus link naming rule, checked by evaluation rather than by booting.
#
# `nix/modules/lingrep-web/test.nix` proves the unit starts, but it needs KVM, so CI
# reaches only its driver build. This check is pure evaluation: it reads the
# generated `ExecStart` and asserts what the corpus arguments point at.
#
# What it guards is a silent failure. lingrep-web infers a source's reader from its
# path extension, so a StateDirectory link named after the attrset key alone
# handed a `.jsonl` corpus to the Markdown dialect — one file became one
# document, one JSONL line became one sentence, and nothing errored. See

{ pkgs, package, ... }:

let
  lib = pkgs.lib;

  system = pkgs.nixos (
    { ... }:
    {
      imports = [
        ./options.nix
        ./nixos.nix
      ];

      # Enough of a system to evaluate. Nothing here is built or booted; the
      # check reads one string off the generated unit.
      boot.loader.grub.enable = false;
      fileSystems."/" = {
        device = "/dev/null";
        fsType = "ext4";
      };
      system.stateVersion = lib.trivial.release;

      services.lingrep-web = {
        enable = true;
        package = package;
        dictionary = "/etc/lingrep-web/dummy.dic";
        corpora = {
          # A source with an extension: the case that broke.
          default = {
            path = "/srv/corpora/science-corpus.jsonl";
          };
          # A source without one — a Markdown document directory, which is
          # what an extensionless path already resolved to.
          notes = {
            path = "/srv/corpora/notes";
          };
          # A prepared artifact, recognized structurally rather than by name.
          bccwj = {
            path = "/srv/corpora/bccwj.g1.a1.deadbeef.locally-executed.lingcorpus";
          };
        };
        patterns.catalogs = [ ];
      };
    }
  );

  # `escapeShellArgs` quotes any token carrying `=`, so a `--corpus NAME=PATH`
  # argument arrives single-quoted and `--input PATH` does not. Strip the
  # quotes rather than write two shapes of expectation, which would pin the
  # escaping and not the naming.
  execStart = lib.replaceStrings [ "'" ] [ "" ] (
    toString system.config.systemd.services.lingrep-web.serviceConfig.ExecStart
  );

  expectations = [
    {
      name = "a source keeps its extension, so format inference sees the corpus the operator declared";
      needle = "--input /var/lib/lingrep-web/corpora/default.jsonl";
    }
    {
      name = "an extensionless source is linked under the bare key, unchanged";
      needle = "--corpus notes=/var/lib/lingrep-web/corpora/notes";
    }
    {
      name = "an artifact takes the same rule rather than a second one";
      needle = "--corpus bccwj=/var/lib/lingrep-web/corpora/bccwj.lingcorpus";
    }
  ];

  failures = lib.filter (expectation: !(lib.hasInfix expectation.needle execStart)) expectations;
in
assert
  failures == [ ]
  || throw ''
    lingrep-web ExecStart does not carry the expected corpus links.

    Missing:
    ${lib.concatMapStringsSep "\n" (
      expectation: "  - ${expectation.name}\n      expected to find: ${expectation.needle}"
    ) failures}

    ExecStart was:
    ${execStart}
  '';
pkgs.runCommand "lingrep-web-corpus-link-naming" { } ''
  touch "$out"
''
