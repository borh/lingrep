# Smoke test for the lingrep-web NixOS module.
#
# Boots a NixOS VM with the module enabled and a minimal corpus/dictionary
# substitute (a tiny JSONL file and a placeholder dictionary path). The test
# asserts the unit starts and the index page responds. Full semantic coverage
# of lingrep-web lives in the lingrep-web Rust suite — this test only proves the unit
# and its `ExecStart` survive the systemd entrypoint.
#
# The check is buildable in any environment with `nix build`; booting the VM
# needs KVM, which may be unavailable in CI sandboxes. CI runs
# `nix flake check --no-build` and the build of the driver; on machines with
# KVM, `nix build .#checks.x86_64-linux.lingrep-web-nixos-smoke.driver &&
# ./result/bin/nixos-test-driver` boots the VM end-to-end.

{ pkgs, package, ... }:

pkgs.testers.nixosTest {
  name = "lingrep-web-smoke";

  meta = {
    maintainers = [ ];
    platforms = [ "x86_64-linux" ];
  };

  nodes.machine =
    { config, ... }:
    {
      imports = [
        ./options.nix
        ./nixos.nix
      ];

      environment.etc."lingrep-web/dummy.dic".source = pkgs.writeText "lingrep-web-dummy.dic" "";

      # A two-line JSONL corpus. Enough to prove format inference at startup.
      environment.etc."lingrep-web/corpora/main.jsonl".text = ''
        {"text": "hello"}
        {"text": "world"}
      '';

      services.lingrep-web = {
        enable = true;
        package = package;
        dictionary = "/etc/lingrep-web/dummy.dic";
        corpora.default = {
          path = "/etc/lingrep-web/corpora/main.jsonl";
        };
        patterns.catalogs = [ ];
        settings.basePath = "/mounted";
      };

      # lingrep-web binds loopback by default; Caddy/Nginx would front it for any
      # real deployment. The test curls directly from inside the VM.
      virtualisation.forwardPorts = [
        {
          from = "host";
          host.port = 3000;
          guest.port = 3000;
        }
      ];
    };

  testScript = ''
    machine.wait_for_unit("lingrep-web.service")
    machine.wait_for_open_port(3000)
    machine.succeed("curl --fail --silent http://127.0.0.1:3000/mounted")
    machine.fail("curl --fail --silent http://127.0.0.1:3000/")
  '';
}
