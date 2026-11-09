{
  pkgs,
  package,
  ling,
  dictionary,
  ...
}:

let
  countTableFixture =
    pkgs.runCommand "lingrep-web-count-table-fixture" { nativeBuildInputs = [ ling ]; }
      ''
        export HOME="$TMPDIR"
        mkdir -p "$out/catalog"
        printf '%b' \
          '# newdoc id = d1\n# text = 猫が鳴く。\n' \
          '1\t猫\t猫\tNOUN\t_\t_\t3\tnsubj\t_\t_\n' \
          '2\tが\tが\tADP\t_\t_\t1\tcase\t_\t_\n' \
          '3\t鳴く\t鳴く\tVERB\t_\t_\t0\troot\t_\t_\n' \
          '4\t。\t。\tPUNCT\t_\t_\t3\tpunct\t_\t_\n\n' > fixture.conllu
        printf 'key: [lemma]\npattern noun := [upos=NOUN].\n' > "$out/catalog/nouns.lx"
        ling corpus build --input-format conllu \
          --output "$out/fixture.lingcorpus" fixture.conllu
        ling summarize --patterns "$out/catalog/nouns.lx" \
          --corpus "$out/fixture.lingcorpus" --write-table "$out/fixture.lingtable"
      '';
  readingSidecarFixture =
    pkgs.runCommand "lingrep-web-reading-sidecar-fixture" { nativeBuildInputs = [ ling ]; }
      ''
        export HOME="$TMPDIR"
        mkdir -p papers "$out"
        printf '# 序論\n\n猫が鳴く。\n' > papers/a.md
        ling corpus build --dict-path ${dictionary}/share/vibrato/unidic-cwj-202512.dic \
          --output "$out/papers.lingcorpus" papers
        ling corpus render "$out/papers.lingcorpus" --source-root papers \
          --output "$out/papers.reading"
      '';
in
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
        corpora.counted = {
          path = "${countTableFixture}/fixture.lingcorpus";
        };
        corpora.papers = {
          path = "${readingSidecarFixture}/papers.lingcorpus";
        };
        patterns.catalogs = [ "${countTableFixture}/catalog" ];
        countTables = [ "${countTableFixture}/fixture.lingtable" ];
        readingSidecars = [ "${readingSidecarFixture}/papers.reading" ];
        metadataLabels = [ "${package}/share/lingrep-web/labels/soranoha-catalog.json" ];
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
    machine.succeed("curl --fail --silent http://127.0.0.1:3000/mounted/")
    machine.fail("curl --fail --silent http://127.0.0.1:3000/")
    machine.succeed("curl --fail --silent http://127.0.0.1:3000/mounted/frequencies")
    meta = machine.succeed("curl --fail --silent http://127.0.0.1:3000/mounted/api/frequency/meta")
    assert '"catalog":"catalog/nouns.lx"' in meta, meta
    assert '"orthographic_style":{"en":"orthography"' in meta, meta
    reader = machine.wait_until_succeeds("curl --fail --silent http://127.0.0.1:3000/mounted/api/read/papers/0/0/1/document")
    assert "data-doc-reader" in reader, reader
    machine.succeed("systemctl stop lingrep-web.service")
    status = machine.succeed("systemctl show --property=ExecMainStatus --value lingrep-web.service")
    assert status.strip() == "0", status
  '';
}
