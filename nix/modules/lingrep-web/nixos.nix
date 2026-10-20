# NixOS module for the lingrep-web pattern-matching web interface.
#
# Builds a systemd unit that runs lingrep-web under a fixed user with
# `ProtectSystem=strict`, exposes the dictionary and corpora as
# `ReadOnlyPaths=`, and assembles the operator's pattern catalogs and
# corpora under the StateDirectory before the unit starts. Catalogs become
# real directories of file symlinks (lingrep-web's discovery walk never follows
# directory symlinks); corpora become symlinks so the content-addressed
# artifact lingrep-web writes beside its input *source* lands in writable
# state, not beside the read-only operator source. A corpus that is
# already a prepared `.lingcorpus` artifact takes neither half of that:
# lingrep-web reads it in place, writes nothing beside it, and never deletes
# it, because it is reproducible from nothing lingrep-web holds. The unit's
# bind address defaults to loopback; the recommended deployment is
# "loopback + reverse proxy on the same host" with `services.caddy` or
# `services.nginx` fronting the unit.

{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.lingrep-web;

  # One entry per catalog: (unique basename, source path).
  # Two catalogs from the same store path with different passAsFile
  # contexts would collide on basename; refuse that here so the operator
  # sees the problem at eval, not at activation.
  catalogEntries = lib.imap1 (
    index: catalog:
    let
      srcPath = toString catalog;
      base = baseNameOf srcPath;
    in
    if lib.hasInfix "/" base then
      throw "services.lingrep-web.patterns.catalogs[${toString index}] = ${srcPath} \
            \u2014 basename contains '/', refusing ambiguous symlink target"
    else
      {
        inherit base;
        src = srcPath;
      }
  ) cfg.patterns.catalogs;

  # Detect basename collisions; conflicting declarations are refused.
  catalogBasenames = map (entry: entry.base) catalogEntries;
  duplicateBasenames = lib.filter (
    name: lib.length (lib.filter (n: n == name) catalogBasenames) > 1
  ) (lib.unique catalogBasenames);

  # An artifact lingrep-web reads in place, recognised by the canonical name
  # `lingrep_corpus::canonical_artifact_path` produces. This is a *name* test, not
  # `is_prepared_artifact`'s structural one: the module cannot stat a path
  # outside the store at eval time. It therefore catches the artifact an
  # operator actually has — the one `corpus build` published — and misses a
  # renamed one, which lingrep-web then refuses at startup instead.
  isCanonicalArtifact = entry: lib.hasSuffix ".lingcorpus" (toString entry.path);

  artifactsWithFormat = lib.filterAttrs (
    _: entry: isCanonicalArtifact entry && entry.format != null
  ) cfg.corpora;

  hasDefaultCorpus = cfg.corpora ? "default";
  defaultEntry = cfg.corpora.default or null;
  otherEntries = lib.filterAttrs (n: _: n != "default") cfg.corpora;

  # Refuse `format` on non-default corpora at eval time. `--input-format` is
  # a single global override; lingrep-web honors it for the `--input` corpus
  # only. An operator who sets `format` on a non-default entry has probably
  # assumed per-corpus format override, which the engine does not provide.
  nonDefaultFormatViolations = lib.filterAttrs (_: entry: entry.format != null) otherEntries;

  # Build the corpora command-line segment. Each conditional piece is a
  # flat list of strings so `lib.escapeShellArgs` sees one element per
  # token. (Do not use `lib.optional cond [ … ]` here: `lib.optional` wraps
  # its value in a list, producing `[ [ … ] ]`, and `lib.escapeShellArgs`
  # then folds the inner list into a single quoted argv token.)
  #
  # `--input`/`--corpus` point at the StateDirectory symlinks, not the
  # operator sources: lingrep-web writes its prepared artifact *beside* the
  # input path (`<name>.g<gen>.<id>.lingcorpus`), and with
  # `ProtectSystem=strict` only the StateDirectory is writable.
  #
  # The link carries the *source's extension*, not the bare key. lingrep-web infers
  # a source's reader from its path extension
  # (`lingrep_core::corpus::resolve_input_format`), so linking under the key alone
  # made `science.path = ".../science-corpus.jsonl"` arrive as
  # `--corpus science=/var/lib/lingrep-web/corpora/science` — an extensionless path,
  # which resolves to the Markdown dialect. The JSONL file was then read as
  # prose: one file became one document and one JSONL line became one sentence,
  # so a 6,371-document corpus served as 6,371 sentences in 1 document, with
  # nothing failing. The key stays the wire identity; only the on-disk name
  #
  # A prepared artifact does not need this — it is recognized structurally, by
  # its `manifest.json` — but it takes the same rule anyway, because one naming
  # rule cannot disagree with itself and an artifact link named `.lingcorpus`
  # says what it points at.
  corpusSuffix =
    entry:
    let
      parts = lib.splitString "." (baseNameOf (toString entry.path));
    in
    if lib.length parts > 1 then ".${lib.last parts}" else "";

  corpusStatePath = name: entry: "/var/lib/lingrep-web/corpora/${name}${corpusSuffix entry}";

  corpusArgs =
    (
      if hasDefaultCorpus then
        [
          "--input"
          (corpusStatePath "default" defaultEntry)
        ]
      else
        [ ]
    )
    ++ (
      if hasDefaultCorpus && defaultEntry.format != null then
        [
          "--input-format"
          defaultEntry.format
        ]
      else
        [ ]
    )
    ++ lib.concatLists (
      lib.mapAttrsToList (name: entry: [
        "--corpus"
        "${name}=${corpusStatePath name entry}"
      ]) otherEntries
    );

  # Compose the full command-line.
  #
  # `--md-include-sections` is the one flag lingrep-web refuses in its empty
  # spelling: an empty include list admits nothing, so the binary demands
  # the flag be *omitted* to admit every label. Omitting it is exactly what
  # an empty operator list means, so the flag is conditional. Empty
  # `--md-exclude-sections` and `--md-self-labels` are legal (they disable
  # exclusion / self-labeling), so those stay unconditional.
  mdIncludeArgs =
    if cfg.markdown.includeSections != [ ] then
      [
        "--md-include-sections"
        (lib.concatStringsSep "," cfg.markdown.includeSections)
      ]
    else
      [ ];

  baseArgs = [
    "--dict-path"
    (toString cfg.dictionary)
    "--host"
    cfg.settings.host
    "--port"
    (toString cfg.settings.port)
    "--base-path"
    cfg.settings.basePath
    "--pattern-root"
    "/var/lib/lingrep-web/patterns"
    "--max-kwic-lines-per-pattern"
    (toString cfg.settings.maxKwicLinesPerPattern)
    "--max-active-sessions"
    (toString cfg.settings.maxActiveSessions)
    "--max-record-bytes"
    (toString cfg.settings.maxRecordBytes)
    "--md-exclude-sections"
    (lib.concatStringsSep "," cfg.markdown.excludeSections)
    "--md-self-labels"
    (lib.concatStringsSep "," cfg.markdown.selfLabels)
  ]
  ++ mdIncludeArgs;

  # `--patterns` is the editor seed, forwarded verbatim and so resolved by
  # lingrep-web against its working directory (`/var/lib/lingrep-web`) — NOT against
  # `--pattern-root`. The assembled root is `<state>/patterns/` and each
  # catalog sits under its own basename inside it, so the value an operator
  # wants is `patterns/<catalog-basename>/<file>.lx`; for a catalog
  # directory itself named `patterns`, the doubled component is real.
  #
  # We only pass it when `editorSeed` is set; otherwise lingrep-web uses its own
  # default (`patterns/sentence-final.lx`, same working-directory rule),
  # which lands inside the assembled root only by coincidence of naming and
  # otherwise produces an "empty editor" warning. That is the documented
  # fallback; auto-resolving "first *.lx of the first catalog" at eval time
  # requires the catalog to be built, which is not guaranteed at evaluation.
  seedArgs =
    if cfg.patterns.editorSeed != "" then
      [
        "--patterns"
        cfg.patterns.editorSeed
      ]
    else
      [ ];
in
{
  imports = [ ./options.nix ];

  config = lib.mkIf cfg.enable {

    # Refuse misconfigurations at eval time rather than at activation or
    # first-request time. The activation script would catch some of these,
    # but failing earlier keeps the operator in their editor.
    assertions = [
      {
        # Structural self-check: every ExecStart element must be a single
        # shell token. A nested list (`lib.optional cond [ … ]`) would make
        # `lib.escapeShellArgs` fold flag+value into one quoted argv element
        # and lingrep-web's parser would refuse to start. Checking at evaluation
        # prevents activation of a broken generation.
        assertion = lib.all (t: builtins.isString t) (baseArgs ++ seedArgs ++ corpusArgs);
        message =
          "services.lingrep-web: ExecStart argument lists contain nested "
          + "lists; every element must be a single shell token. Use "
          + "`if cond then [ … ] else [ ]`, not `lib.optional cond [ … ]` "
          + "(which wraps its value in a list).";
      }
      {
        # lingrep-web requires at least one corpus, but the `default` entry is
        # optional: `--input` is reserved for it, while a server with only
        # named `--corpus` entries is valid.
        assertion = lib.length (lib.attrNames cfg.corpora) >= 1;
        message = "services.lingrep-web.corpora must declare at least one corpus.";
      }
      {
        assertion = duplicateBasenames == [ ];
        message = "services.lingrep-web.patterns.catalogs has duplicate basename(s): \
                  \  ${lib.concatStringsSep ", " duplicateBasenames}. \
                  \  Rename the package or path so the basenames are unique.";
      }
      {
        # `format` is global: lingrep-web applies it via `--input-format`, which
        # only the default corpus reaches. Refuse format on non-default
        # entries at eval time so the operator learns the limitation before
        # the unit silently ignores it.
        assertion = nonDefaultFormatViolations == { };
        message = "services.lingrep-web.corpora: `format` only applies to the \
                  \ `default` corpus (it forwards as `--input-format`, a \
                  \ global override). The following non-default entries \
                  \ set it and would be silently ignored: \
                  \  ${lib.concatStringsSep ", " (lib.attrNames nonDefaultFormatViolations)}.";
      }
      {
        # lingrep-web refuses `--input-format` for a prepared artifact rather than
        # ignoring it: an artifact records the reader it was built with, and
        # reader configuration reaches artifact identity, so an operator who
        # passes both has every reason to believe it took effect. The unit
        # would exit 2 at startup and crash-loop; refuse at eval instead.
        assertion = artifactsWithFormat == { };
        message = "services.lingrep-web.corpora: `format` does not apply to a \
                  \ prepared `.lingcorpus` artifact. An artifact records the \
                  \ reader it was built with and is read directly, so lingrep-web \
                  \ refuses `--input-format` for one and the unit would fail \
                  \ to start. Drop `format` from: \
                  \  ${lib.concatStringsSep ", " (lib.attrNames artifactsWithFormat)}.";
      }
      {
        # `--input-format` is a single global override; lingrep-web refuses it
        # with >1 corpus. Mirror that here so the operator sees one error,
        # not two.
        assertion =
          (lib.length (lib.attrNames cfg.corpora) <= 1)
          || (lib.all (c: c.format == null) (lib.attrValues cfg.corpora));
        message = "services.lingrep-web.corpora: `default.format` is a single global \
                  \  override and lingrep-web refuses it with multiple corpora. \
                  \  Drop `format` from the `default` entry and let lingrep-web \
                  \  infer per path.";
      }
    ];

    users.users = lib.mkIf (cfg.user == "lingrep-web") {
      lingrep-web = {
        isSystemUser = true;
        group = cfg.group;
        home = "/var/lib/lingrep-web";
      };
    };

    users.groups = lib.mkIf (cfg.group == "lingrep-web") { lingrep-web = { }; };

    systemd.services.lingrep-web = {
      description = "lingrep-web pattern-matching web interface";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      serviceConfig = {
        ExecStart = lib.escapeShellArgs (
          [ (lib.getExe cfg.package) ] ++ baseArgs ++ seedArgs ++ corpusArgs
        );
        User = cfg.user;
        Group = cfg.group;

        StateDirectory = "lingrep-web";
        StateDirectoryMode = "0750";
        WorkingDirectory = "/var/lib/lingrep-web";

        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        NoNewPrivileges = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        ProtectHostname = true;
        ProtectClock = true;
        RestrictNamespaces = true;
        LockPersonality = true;
        SystemCallArchitectures = "native";

        # lingrep-web reads the dictionary and corpora but never writes them.
        # StateDirectory holds the farms and lingrep-web's artifact output; the
        # operator sources stay read-only.
        ReadOnlyPaths = [
          cfg.dictionary
        ]
        ++ lib.mapAttrsToList (_: entry: entry.path) cfg.corpora;

        Restart = "on-failure";
        RestartSec = "5s";

        Environment = lib.mapAttrsToList (n: v: "${n}=${v}") cfg.environment;
      };
    };

    # Assemble the farms before the unit starts. This uses an activation
    # script rather than a `pkgs.runCommand` farm because sources may
    # live on persistent storage the operator manages outside the Nix
    # store.
    #
    # Catalogs: a real directory per catalog, filled with symlinks to the
    # catalog's `.lx` files (relative layout preserved). A directory
    # symlink would be invisible to lingrep-web's discovery walk, which never
    # follows directory symlinks.
    #
    # Corpora: one symlink per corpus. lingrep-web resolves the input path as
    # given (no canonicalization), so its artifact lands beside the
    # symlink, inside the StateDirectory.
    system.activationScripts.lingrep-web-patterns = {
      text = ''
        set -e
        mkdir -p /var/lib/lingrep-web/patterns
        ${lib.concatMapStringsSep "\n" (entry: ''
          rm -rf /var/lib/lingrep-web/patterns/${entry.base}
          mkdir -p /var/lib/lingrep-web/patterns/${entry.base}
          (
            cd ${entry.src}
            find . \( -type f -o -type l \) -name '*.lx' -print0 |
            while IFS= read -r -d ''' rel; do
              dest="/var/lib/lingrep-web/patterns/${entry.base}/$rel"
              mkdir -p "$(dirname "$dest")"
              ln -sf "$(pwd)/$rel" "$dest"
            done
          )
        '') catalogEntries}
        mkdir -p /var/lib/lingrep-web/corpora
        # The activation runs as root; the unit's user must create the
        # content-addressed artifact beside its corpus symlink.
        chown ${cfg.user}:${cfg.group} /var/lib/lingrep-web/corpora
        ${lib.concatStringsSep "\n" (
          lib.mapAttrsToList (name: entry: ''
            ln -sfn ${entry.path} ${corpusStatePath name entry}
          '') cfg.corpora
        )}
      '';
    };

    networking.firewall.allowedTCPPorts = lib.mkIf cfg.settings.openFirewall [ cfg.settings.port ];
  };
}
