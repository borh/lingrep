# lingrep patterns browser assets

The grammar WASM and highlight query are generated together from the exact
`tree-sitter-lingrep` revision pinned in `Cargo.lock`:

```sh
nix develop --command bash lingrep-web/assets/editor/regenerate.sh
```

The script uses a temporary checkout, updates measured SHA-256 values in the
manifest, and does not modify Cargo dependency files.
