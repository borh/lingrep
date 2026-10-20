# lingrep-web deployment guide

This guide explains how to run `lingrep-web` as a NixOS service behind a reverse
proxy (Caddy is the worked example; Nginx follows the same shape). The
recommended deployment is loopback plus a reverse proxy on the same host:
lingrep-web binds only `127.0.0.1`, Caddy terminates TLS, and access control
(basic auth, OIDC, IP allowlists) is configured at the proxy.

## Why loopback + reverse proxy

`lingrep-web` has no built-in authentication, rate limiting, or TLS. Bound
directly to a routable interface, every endpoint is reachable without access
control: every declared corpus is readable, every pattern catalog is
browseable, and any client can submit search requests without limit. The
systemd unit provides process isolation; transport-layer protection is the
operator's responsibility.

## Minimal NixOS configuration

```nix
{ inputs, pkgs, ... }:

let
  lingrepPkgs = inputs.lingrep.packages.${pkgs.stdenv.hostPlatform.system};
in
{
  imports = [ inputs.lingrep.nixosModules.lingrep-web ];

  services.lingrep-web = {
    enable = true;
    package = lingrepPkgs.lingrep-web;
    dictionary = "${lingrepPkgs.vibrato-dict-cwj}/share/vibrato/unidic-cwj-202512.dic";
    corpora.default = {
      path = "/srv/corpora/main.jsonl";
    };
    patterns.catalogs = [
      (inputs.lingrep.outPath + "/patterns")
    ];
  };

  # Open the firewall only if the proxy lives on a different host. With
  # loopback + local proxy, leave this off.
  # services.lingrep-web.settings.openFirewall = false;
}
```

Write each corpus `path` as a Nix string; an unquoted path literal copies the
corpus into the store.

lingrep-web binds `127.0.0.1:3000` by default. The NixOS module's systemd unit
runs as the system user `lingrep-web` with `ProtectSystem=strict`; corpora and
the dictionary are mounted with `ReadOnlyPaths=`, and an activation script
symlinks corpora and pattern catalogs under `/var/lib/lingrep-web/`.

## Serving under a subpath

Set `services.lingrep-web.settings.basePath` when the application is mounted below
the proxy's root. The value may be a nested path such as
`/temp-lingrep-web-proxy`, without a trailing slash. lingrep-web serves its
routes under that prefix and emits browser-relative links, requests, assets,
and permalinks, so one process can serve both a public subpath and a second
proxy that rewrites its own root into that prefix.

```nix
services.lingrep-web.settings.basePath = "/temp-lingrep-web-proxy";
```

For a public subpath, redirect the no-slash path and preserve the prefix while
proxying:

```caddyfile
redir /temp-lingrep-web-proxy /temp-lingrep-web-proxy/
route /temp-lingrep-web-proxy/* {
  reverse_proxy 127.0.0.1:3000
}
```

For a root-facing virtual host that shares the same process, rewrite each
request to the internal mount before proxying:

```caddyfile
route {
  rewrite * /temp-lingrep-web-proxy{uri}
  reverse_proxy 127.0.0.1:3000
}
```

## Caddy vhost

```caddyfile
lingrep.example.com/v1 {
  encode zstd gzip
  reverse_proxy 127.0.0.1:3000 {
    flush_interval -1
    # lingrep-web accepts API request bodies up to 4 MiB.
    max_request_body 4 * 1024 * 1024
  }
  basicauth {
    alice $2a$14$…  # `caddy hash-password`
  }
}
```

- `flush_interval -1` is required. lingrep-web publishes progress and exact
  counts for the scanned prefix over Server-Sent Events; without it, Caddy
  buffers 4 KiB before forwarding and the UI shows no progress until the
  buffer fills.
- Basic auth is the simplest access control. Caddy also supports OIDC through
  the `caddy-security` plugin.

## Nginx equivalent

```nginx
server {
  server_name lingrep.example.com/v1;
  listen 443 ssl http2;
  ssl_certificate /var/lib/acme/lingrep.example.com/v1/fullchain.pem;
  ssl_certificate_key /var/lib/acme/lingrep.example.com/v1/key.pem;

  location / {
    proxy_pass http://127.0.0.1:3000;
    proxy_http_version 1.1;
    proxy_buffering off;        # SSE flushes
    proxy_read_timeout 1h;       # long-lived SSE connections
    client_max_body_size 4m;     # matches lingrep-web's API body cap
  }

  auth_basic "lingrep-web";
  auth_basic_user_file /etc/nginx/.lingrep-web.htpasswd;
}
```

## Multiple corpora

```nix
services.lingrep-web.corpora = {
  default = { path = "/var/lib/lingrep-web/corpora/main.jsonl"; };
  sci = { path = "/var/lib/lingrep-web/corpora/science/"; format = "markdown"; };
};
```

The entry named `default` is passed as `--input`; every other entry is passed
as `--corpus NAME=PATH`. Names match `[A-Za-z0-9_-]{1,64}`. The UI's corpus
selector starts at `k=default`, and the selection is encoded in the URL, so a
link to a result reproduces the corpus and pattern pair.

`format` is forwarded as `--input-format`, a single global override, so it is
accepted only on the `default` entry and only when one corpus is declared.
With several corpora, leave `format` unset and lingrep-web infers each
corpus's format from its path. The module checks this at evaluation time.

`default` is optional. When only named corpora are declared, the first in
attribute-set order is the default; Nix sorts attribute names, so
`bccwj` comes before `science` regardless of their order in the file. Only the
default corpus is prepared at startup; every other corpus is prepared on first
selection.

## Prepared artifacts, and how BCCWJ is served

A `corpora.<name>.path` is either a corpus source or a prepared V1 artifact,
a directory containing a `manifest.json`. lingrep-web distinguishes them by
that file; the module has no option for it.

lingrep-web reads a declared artifact in place and never writes beside it or
deletes it. If a declared artifact is corrupt, the corpus is marked failed and
its files are left unchanged. An artifact that lingrep-web built itself from a
source is discarded and rebuilt when corrupt.

A prepared artifact is how lingrep-web serves a format it has no reader for.
BCCWJ is read only by `ling`, and lingrep-web refuses a BCCWJ distribution
root. Prepare it once, then declare the result:

```sh
# On the deployment host, as a user who can write beside the root:
ling corpus build \
  --input-format bccwj \
  --dict-path "$LINGREP_DICTIONARY_PATH" \
  /data/bccwj
# publishes /data/bccwj.g2.a1.<analysis-identity>.locally-executed.lingcorpus
```

```nix
services.lingrep-web.corpora.bccwj = {
  # The artifact directory, not the distribution root. Leave `format`
  # unset: lingrep-web refuses `--input-format` for an artifact, and the
  # module rejects it at evaluation time for a path ending in `.lingcorpus`.
  path = "/data/lingrep-web/corpora/bccwj.g2.a1.<identity>.locally-executed.lingcorpus";
};
```

The artifact name embeds an analysis identity that changes with the dictionary
and analyzer, so rebuilding under a new dictionary produces a new directory
beside the old one. Update `path` to the new directory and delete the old one.

At startup lingrep-web checks a declared artifact's manifest and component
lengths without hashing its contents. Run `ling corpus inspect --verify` for a
complete check.

## Pattern catalogs

`patterns.catalogs` accepts paths or Nix packages, each a directory searched
recursively for `*.lx` files. The module's activation script symlinks each
catalog under `/var/lib/lingrep-web/patterns/<basename>/` and passes that root
as `--pattern-root`.

The editor seed is not resolved against that root. `editorSeed` is
forwarded verbatim as `--patterns`, which lingrep-web resolves against its
working directory, `/var/lib/lingrep-web`. So the value to write is
`patterns/<catalog-basename>/<file>.lx`, including the leading `patterns/`:

```nix
services.lingrep-web.patterns.editorSeed = "patterns/lingrep-patterns-default/sf-experimental.lx";
```

A catalog directory that is itself named `patterns` — this repository's
own `patterns/`, passed as `inputs.lingrep.outPath + "/patterns"` —
is therefore at `patterns/patterns/sf-experimental.lx`.

When `editorSeed` is empty, the default, lingrep-web reads
`patterns/sentence-final.lx` relative to its working directory. A seed that
resolves to no file only logs an "empty editor" warning, so check the unit's
log after changing it.

Two catalogs with the same basename fail a module assertion at
`nixos-rebuild` time. Rename the package or
directory so the basenames are unique.

## Dictionary preparation

The flake ships seven UniDic 202512 variants:

```sh
nix build .#vibrato-dict-cwj --no-link --print-out-paths
# → /nix/store/…-vibrato-dict-cwj-202512
# use: /nix/store/…-vibrato-dict-cwj-202512/share/vibrato/unidic-cwj-202512.dic
```

See [`DICTIONARIES.md`](../DICTIONARIES.md) for the full list. lingrep-web
opens the dictionary on first corpus selection, not at startup.

## Markdown corpus dialect

lingrep-web reads `.md` files using the pandoc dialect (one sentence per line).
Section selection is applied at read time and changes which lines become
sentences, so the same file read under two section policies produces two
different artifacts. The module's default, `markdown.excludeSections = [
"references" ]`, excludes only `references`; lingrep-web run without the
module excludes `references` and ten other labels. Set `excludeSections = []`
to read unfiltered text. See [the dialect contract](markdown-corpus-dialect.md)
for the section labels.

## Tuning

| Option | Default | When to raise |
|---|---|---|
| `settings.maxKwicLinesPerPattern` | 1000 | A catalog whose matches include rare patterns with very long KWIC lists |
| `settings.maxActiveSessions` | 256 | Concurrent research use; raise or lower to bound memory |
| `settings.maxRecordBytes` | 1 MiB | Markdown or JSONL documents that exceed 1 MiB |
| `markdown.excludeSections` | `[ "references" ]` | `[]` reads unfiltered text |

Each option maps to the lingrep-web flag of the same name.

## Authentication, rate limiting, and state

- Authentication and rate limiting: lingrep-web provides neither; configure
  them at the proxy.
- State: lingrep-web writes the artifacts it prepares from corpus sources under
  `/var/lib/lingrep-web/corpora/`. They are rebuildable from the sources, and
  sessions are not persisted, so lingrep-web has no state requiring backup.

## Logs and observability

The unit writes to journald:

```sh
journalctl -u lingrep-web -f
```

`RUST_LOG` defaults to `info`. Raise it to `debug` to trace corpus
preparation:

```nix
services.lingrep-web.environment.RUST_LOG = "info,lingrep_web=debug";
```

## Development workflow

For local development, run `cargo run -p lingrep-web` in the dev shell, which
exports `LINGREP_DICTIONARY_PATH`. After editing the NixOS module, run
`nix flake check`.
