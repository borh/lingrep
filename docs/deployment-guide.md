# lingrep-web deployment guide

This guide explains how to run `lingrep-web` as a NixOS service behind a reverse
proxy (Caddy is the worked example; Nginx follows the same shape). The
recommended deployment is loopback plus a reverse proxy on the same host:
`lingrep-web` binds only `127.0.0.1`, Caddy terminates TLS, and access control
(basic auth, OIDC, IP allowlists) is configured at the proxy.

Without Nix, `ling serve` from the release archives runs the same server with
the same arguments as `lingrep-web`; the proxy guidance below applies to it
unchanged.

## Bind loopback behind a reverse proxy

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

`lingrep-web` binds `127.0.0.1:3000` by default. The NixOS module's systemd unit
runs as the system user `lingrep-web` with `ProtectSystem=strict`; corpora and
the dictionary are mounted with `ReadOnlyPaths=`, and an activation script
symlinks corpora and pattern catalogs under `/var/lib/lingrep-web/`.

On SIGTERM, which `systemctl stop` sends, `lingrep-web` stops accepting
connections and lets in-flight requests and open progress streams run for up
to 10 seconds. It then cancels running scans and waits up to 3 seconds for
them, stops a running corpus build and waits up to 10 seconds for it to remove
its staging directory, and exits 0. A second signal ends these waits at once.
`ling serve` stops the same way on SIGTERM or SIGINT.

## Serving under a subpath

Set `services.lingrep-web.settings.basePath` when the application is mounted below
the proxy's root. The value may be a nested path such as
`/temp-lingrep-web-proxy`, without a trailing slash. `lingrep-web` serves its
routes under that prefix and emits browser-relative links, requests, assets,
and permalinks, so one process can serve both a public subpath and a second
proxy that rewrites its own root into that prefix. The start page is at the
prefix with a trailing slash, `/temp-lingrep-web-proxy/`, where those relative
links resolve inside the prefix; `lingrep-web` redirects the bare prefix there.

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

## Question documents

The start page is a question document with linked investigations, continuous
concordance, document conditions, and expandable sentence context. The pattern
workbench is served at `workbench`; the Frequencies, Compare, and Sample pages
are served at `frequencies`, `compare`, and `sample`.

Saved question files preserve queries, document conditions, corpus identities,
and the links between investigations. Results belong to the current server
session and can expire when the server reclaims old sessions or restarts.
Reopening a saved document then offers to rerun its queries; it does not run
them automatically. A corpus identity mismatch stops the rerun without searching
different data.

A saved question with a `near` test records the BLAKE3 digest of its word-vector
table. Replaying it on a server with different vectors stops before scanning.
Choose “Search with the current word vectors” to run it with the replacement
table. A server without vectors reports the digest that the saved query requires. Each linked
search retains its own table identity. A completed search that uses vectors also
includes `vectors_blake3` in the run record of its JSON Lines export.

Complete question results use a temporary index capped at 128 MiB per search.
If a search exceeds that limit, narrow its pattern or document conditions and
run it again. The initial preview holds up to 4 MiB of examples. Use the
concordance pages to browse all indexed hits. Sorting uses a separate 128 MiB
index. If sorting exceeds that limit, narrow the query or sort within one
document.

If a row exceeds the display budget, it keeps its place and links to the JSONL
export. The export streams all indexed matches using
`lingrep.question-results/v1`, including sentences above the 1 MiB limit for
loading a preview. Export memory grows with the largest matched sentence.

## Configure live analysis for the Sample debugger

Register the same analyzer configuration that built the corpus to analyze
pasted or edited text interactively. On Unix, pass a trusted spaCy, GiNZA,
Stanza, or protocol adapter configuration as `--sample-analyzer-config`.
The setting applies to the standalone Sample debugger and Workbench's
**Test on sample** drawer.

For a local GiNZA setup, save a few UTF-8 test sentences in `samples.txt`
and run:

```bash
ling analyzer prepare ginza --output ./analyzers/ginza
ling corpus build --analyzer-config "$PWD/analyzers/ginza/analyzer.toml" \
  --output demo.lingcorpus samples.txt
ling serve --corpus demo=demo.lingcorpus \
  --sample-analyzer-config "$PWD/analyzers/ginza/analyzer.toml"
```

Open `http://127.0.0.1:3000/workbench`, select **demo**, search for
`word := [].`, and open a hit in the Sample debugger. Change the pattern to
check the stored tokens. Then edit the sample sentence and check that the
result shows **GiNZA** and **Matches the corpus configuration**.

To configure Sudachi, pass `--sample-sudachi-dict PATH` with a
`system_small.dic`, `system_core.dic`, or `system_full.dic` file. Supply
`--sample-sudachi-resources DIR` and `--sample-sudachi-split a|b|c` when
the corpus was built with custom resources or a split other than `c`.
Use `--sample-nfc` for a live analyzer that must reproduce a build with
`--normalization nfc`. The server accepts one added analyzer configuration
per process. Live samples use the morphology profile and input segmentation
with one sentence per line.

For the NixOS service, set
`services.lingrep-web.environment.LINGREP_WEB_SAMPLE_ANALYZER_CONFIG` to
the absolute path of the bundle configuration. For Sudachi, set
`LINGREP_WEB_SAMPLE_SUDACHI_DICT` in the same environment. The optional
`LINGREP_WEB_SAMPLE_SUDACHI_RESOURCES` and `LINGREP_WEB_SAMPLE_SUDACHI_SPLIT`
variables mirror their CLI flags. Set `LINGREP_WEB_SAMPLE_NFC` to `true`
if the corpus uses NFC. Keep bundle files and the interpreter in a
service-readable location such as `/var/lib/lingrep` or the Nix store. The
unit hides home directories.

Opening a corpus sentence starts with its stored tokens. Pattern edits reuse
those tokens. After a text edit, **Automatic** requires the exact corpus
analyzer identity when the sample is tied to a corpus. Choose a named
analyzer explicitly to analyze the text with a different configuration.
Restoring the original text in **Automatic** restores the stored analysis.
If you open Sample without a corpus, **Automatic** uses the configured analyzer,
or Vibrato when none is configured. For a pretokenized corpus, edit the
pattern on stored tokens or choose a live analyzer for edited text.

The server checks the analyzer configuration at startup and starts the
external process when needed. Give the service read and execute access to
the bundle and interpreter. The TOML file selects executable code, so use
operator-controlled paths and bundles. Timeout and error messages appear
under the sample, where you can shorten the text, retry, or choose another
analyzer.

## Frequencies view

`services.lingrep-web.countTables` lists count tables written by
`ling summarize --write-table`; the module passes each as `--count-table` and
mounts it read-only. A non-empty list adds the Frequencies view at
`frequencies` under the base path, and the Compare page at `compare`. Each
table binds to a prepared `.lingcorpus` corpus and a catalog by the identities
it records, so declare that corpus in `corpora` and its catalog in
`patterns.catalogs`. A table also
records the matcher revision, so write it with the `ling` from the same lingrep
release as the unit's `lingrep-web`:

```nix
services.lingrep-web = {
  corpora.works.path = "/srv/corpora/works.lingcorpus";
  patterns.catalogs = [ (inputs.lingrep.outPath + "/patterns") ];
  countTables = [ "/srv/tables/works.collexeme.lingtable" ];
};
```

```bash
ling summarize --patterns patterns/collexeme.lx \
  --corpus /srv/corpora/works.lingcorpus \
  --write-table /srv/tables/works.collexeme.lingtable
```

A table that does not bind stops the unit at startup; `journalctl -u
lingrep-web` shows the error identifying it. The binding rules and errors are in
[the CLI usage guide](cli-usage-guide.md#frequency-view-from-count-tables).
Rebuild a table after changing its corpus or catalog, and after an upgrade
that the startup error reports as a different matcher revision or count table
version.

Selecting a row opens the entry's charts and lines. The server matches its lines
from the corpus at request time over only the sentences that the table records
for the entry, so the service requires both the corpus and the table. A request
matches only the sentences that hold its page of lines: 50 by default, at most
200.

`services.lingrep-web.metadataLabels` lists display-label files; the module
passes each as `--metadata-labels` and mounts it read-only. The question document,
Frequencies, and Compare use the key labels in English and Japanese; the question
document also uses configured value labels. Clauses, commands, and addresses keep
the raw keys. The package installs the Soranoha catalog labels:

```nix
services.lingrep-web.metadataLabels = [
  "${pkgs.lingrep-web}/share/lingrep-web/labels/soranoha-catalog.json"
];
```

A file that is not a valid label file stops the unit at startup with an error
identifying it. The file format is in
[the CLI usage guide](cli-usage-guide.md#frequency-view-from-count-tables).

The workbench's capture-key picker runs fresh summaries through a companion
`ling` executable beside `lingrep-web`; the Nix package includes it. A CLI whose
matcher semantics differ is rejected. Each live summary counts all admitted
input, shows at most 50 groups per pattern, and uses the selected search scope.
The web path allows 1 GiB of temporary aggregation, a 16 MiB response, and
120 seconds of execution. Use `ling summarize` directly for larger runs.
Capture-key edits in the workbench do not modify saved count tables.

## Rendered documents

`services.lingrep-web.readingSidecars` lists reading sidecars written by
`ling corpus render`; the module passes each as `--reading-sidecar` and mounts
it read-only. The reader displays each rendered document with its headings and
paragraphs. Each sidecar binds to a prepared `.lingcorpus` entry in `corpora` by
the identities that it records, and also records the renderer revision. Render
each sidecar with the same `--md-*` options that built the corpus and with the
`ling` binary from the same lingrep release as `lingrep-web`:

```nix
services.lingrep-web = {
  corpora.papers.path = "/srv/corpora/papers.lingcorpus";
  readingSidecars = [ "/srv/reading/papers.reading" ];
};
```

```bash
ling corpus render /srv/corpora/papers.lingcorpus --source-root /srv/papers \
  --md-recover-conversion-artifacts --output /srv/reading/papers.reading
```

Startup fails and identifies any sidecar that the service cannot load or
bind to a served corpus. The binding rules are in
[the CLI usage guide](cli-usage-guide.md#rendered-documents-from-reading-sidecars).

`api/read`, which the Frequencies view uses to open a line, returns any
sentence of a served corpus, and the document text around it, by its ordinal,
with or without a sidecar. Anyone who can reach the server can therefore read
every document of every served corpus, so put the same access control in
front of the unit as the corpora require.

## Caddy vhost

```caddyfile
lingrep.example.com {
  encode zstd gzip
  request_body {
    max_size 4194304
  }
  reverse_proxy 127.0.0.1:3000
  basic_auth {
    alice $2a$14$…  # Replace with the output of `caddy hash-password`.
  }
}
```

- Leave `flush_interval` unset. Caddy flushes `text/event-stream` responses
  immediately; no extra flush setting is needed. See [Caddy's streaming documentation](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy#streaming).
- A workbench with no open event stream is canceled after a 30-second grace
  period. Reconnecting and leaving starts a new grace period. Keep client-abort
  propagation enabled through every proxy between the browser and `lingrep-web`,
  and check cancellation after changing the deployed proxy configuration or version.
- `request_body` limits requests to the server's 4 MiB API body cap.
- Basic auth is the simplest access control. Caddy also supports OIDC through
  the `caddy-security` plugin.

## Nginx equivalent

```nginx
server {
  server_name lingrep.example.com;
  listen 443 ssl http2;
  ssl_certificate /var/lib/acme/lingrep.example.com/fullchain.pem;
  ssl_certificate_key /var/lib/acme/lingrep.example.com/key.pem;

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
With several corpora, leave `format` unset and `lingrep-web` infers each
corpus's format from its path. The module checks this at evaluation time.

`default` is optional. When only named corpora are declared, the first in
attribute-set order is the default; Nix sorts attribute names, so
`bccwj` comes before `science` regardless of their order in the file. Only the
default corpus is prepared at startup; every other corpus is prepared on first
selection.

## Prepared artifacts, and how BCCWJ is served

A `corpora.<name>.path` is either a corpus source or a prepared V1 artifact,
a directory containing a `manifest.json`. `lingrep-web` distinguishes them by
that file; the module has no option for it.

`lingrep-web` reads declared artifacts in place and never writes beside them.
Any existing artifact that cannot be opened at startup, including an incomplete
copy beside a declared source, is left unchanged and its corpus is marked
failed. Finish or quarantine that copy before restarting; publish complete
artifact directories with an atomic rename rather than copying into a live
artifact path.

If corruption is detected during a read of an artifact that this process
already opened successfully beside a declared source, the server deletes
and rebuilds it once. If the replacement is corrupt, it is deleted and the
corpus fails until restart.

Other open errors leave the artifact unchanged and mark its corpus failed:
permission or I/O errors, missing components, symbolic-link components,
SQLite `-wal`, `-shm` or `-journal` files beside `provenance.sqlite`, or
unsupported manifest or component versions. Correct the cause, or delete
the artifact directory, and restart.

Use a prepared artifact to serve a format that `lingrep-web` cannot read directly.
BCCWJ is read only by `ling`, and `lingrep-web` rejects a BCCWJ distribution
root. Prepare it once, then declare the result:

```sh
# On the deployment host, as a user who can write beside the root:
ling corpus build \
  --input-format bccwj \
  --dict-path "$LINGREP_DICTIONARY_PATH" \
  /data/bccwj
# publishes /data/bccwj.g3.a1.<analysis-identity>.locally-executed.lingcorpus
```

```nix
services.lingrep-web.corpora.bccwj = {
  # The artifact directory, not the distribution root. Leave `format`
  # unset: lingrep-web rejects `--input-format` for an artifact, and the
  # module rejects it at evaluation time for a path ending in `.lingcorpus`.
  path = "/data/lingrep-web/corpora/bccwj.g3.a1.<identity>.locally-executed.lingcorpus";
};
```

The artifact name embeds an analysis identity that changes with the dictionary,
the analyzer, and a `ling` release that changes how tokens are analyzed, so
rebuilding under a new dictionary or such a release produces a new directory
beside the previous directory. Update `path` to the new directory, rebuild its
count tables, and delete the previous directory.

At startup `lingrep-web` checks a declared artifact's manifest and component
lengths without hashing its contents. Run `ling corpus inspect --verify` for a
complete check.

## Interrupted corpus builds

A build writes a sibling directory named `.NAME.staging-PID-N` before publishing
its artifact, and holds an exclusive lock on a sibling file named
`.NAME.stagelock-PID-N` until that directory is published or removed.

A server stopped by SIGTERM or SIGINT stops its builds and waits up to 10
seconds for them to remove their staging directories and lock files before it
exits. A build that is killed, or that has not stopped when that wait ends or
a second signal arrives, leaves its staging directory and lock file behind.
A later build of the same artifact can remove the staging directory in either
of these cases.

- The lock file records the current hostname and the new build can acquire
  the exclusive lock.
- On Unix, the lock file is absent and the new build confirms that the process
  ID in the directory name is absent locally. Releases before 0.8.0 did not
  create lock files. A failed cleanup can also leave a lockless directory.

Use the manual cleanup procedure below for other leftovers, including those
for an artifact whose name changed with the dictionary or analyzer. Run
concurrent builds from a pre-0.8.0 release and a current release in separate
directories if they use different hosts or PID namespaces. A local process ID
check cannot identify a pre-0.8.0 build running in another namespace.
Two hosts that report the same host name must not build into one shared
directory on a filesystem that does not propagate `flock` locks between hosts.

Stop the service and every other corpus builder using the directory before
cleanup. Inspect the candidates; the name alone does not establish that a build
has stopped. Replace the example removal paths with an inspected staging
directory from a stopped build and its lock file. Remove neither published
`.lingcorpus` artifacts nor unrelated directories.

```sh
sudo systemctl stop lingrep-web
sudo find /var/lib/lingrep-web/corpora -maxdepth 1 \( -name '.*.staging-*' -o -name '.*.stagelock-*' \) -print
sudo rm -r -- /var/lib/lingrep-web/corpora/.news.lingcorpus.staging-12345-0
sudo rm -f -- /var/lib/lingrep-web/corpora/.news.lingcorpus.stagelock-12345-0
sudo systemctl start lingrep-web
```

## Pattern catalogs

`patterns.catalogs` accepts paths or Nix packages, each a directory searched
recursively for `*.lx` files. The module's activation script symlinks each
catalog under `/var/lib/lingrep-web/patterns/<basename>/` and passes that root
as `--pattern-root`.

The editor seed is not resolved against that root. `editorSeed` is
forwarded verbatim as `--patterns`, which `lingrep-web` resolves against its
working directory, `/var/lib/lingrep-web`. Write the value as
`patterns/<catalog-basename>/<file>.lx`, including the leading `patterns/`:

```nix
services.lingrep-web.patterns.editorSeed = "patterns/lingrep-patterns-default/sf-experimental.lx";
```

The Sample debugger opens with the seed file only when `editorSeed` is set.
Without it, the Sample debugger opens with a two-line starter pattern that
matches its default sentences, and the Workbench editor opens with
`patterns/sentence-final.lx`.

If a catalog directory is itself named `patterns` (such as this repository's
own `patterns/`, passed as `inputs.lingrep.outPath + "/patterns"`), its files
appear under `patterns/patterns/`, for example
`patterns/patterns/sf-experimental.lx`.

When `editorSeed` is empty, the default, `lingrep-web` reads
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

See [`DICTIONARIES.md`](../DICTIONARIES.md) for the full list. `lingrep-web`
opens the dictionary on first corpus selection, not at startup. A server whose
declared corpora are all prepared artifacts uses the dictionary only for the
Workbench sample test, and starts without one when none resolves.

## Markdown corpus dialect

`lingrep-web` reads `.md` files using the pandoc dialect (one sentence per line).
The section policy used during preparation determines which lines become
sentences, so the same file read under two section policies produces two
different artifacts. The module's default, `markdown.excludeSections = [
"references" ]`, excludes only `references`; `lingrep-web` run without the
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

Each option maps to the `lingrep-web` flag of the same name.

## Authentication, rate limiting, and state

- Authentication and rate limiting: `lingrep-web` provides neither; configure
  them at the proxy.
- `lingrep-web` stores prepared artifacts built by the service under
  `/var/lib/lingrep-web/corpora/`. Keep the original sources and configuration
  needed to rebuild them. Sessions expire when the service restarts.

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

A catalog with a `near` test requires a word-vector file. Configure its path
for the server and the `ling` processes it starts:

```nix
services.lingrep-web.environment.LINGREP_VECTORS_PATH = "/var/lib/lingrep/vectors.vec";
```

For a pinned installation, set the same variable to the absolute directory of a
[word-vector bundle](cli-usage-guide.md#sets-of-similar-words-ling-neighbors).
Install each revision at a new path and restart the service to select it. The
server checks the manifest and table digest at startup. With vectors
configured, the navigation includes a Similar Words page.
