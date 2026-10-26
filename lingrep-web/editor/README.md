# lingrep-web lingrep patterns editor bundle

This directory builds the vendored CodeMirror bundle used by the in-browser
lingrep patterns editor (`assets/editor/pattern-editor.js`). The bundle is committed, so a normal
lingrep-web build requires no npm installation. Rebuild the bundle only when
upgrading CodeMirror.

## Rebuild

```bash
cd lingrep-web/editor
npm ci          # reproduce package-lock.json exactly
npm run build   # esbuild bundle -> ../assets/editor/vendor/codemirror.js
```

Then record the new digest in `assets/editor/vendor/manifest.json`:

```bash
sha256sum ../assets/editor/vendor/codemirror.js
```

`lingrep-web/tests/suite/editor_asset_test.rs` verifies every `sha256` entry in the manifest
against the served bytes, so a stale digest fails the test suite.

## Notes

- Only symbols re-exported from `entry.js` are bundled. Add a CodeMirror API
  there before the editor uses it.
- The bundle is ESM (`--format=esm`), loaded via dynamic `import()` from
  `pattern-editor.js` so the Node-based js-tests never evaluate it.
- CodeMirror injects its structural styles at runtime; the lingrep patterns-specific theme
  lives in `assets/editor/pattern-editor.css` and follows the site light/dark custom
  properties.
