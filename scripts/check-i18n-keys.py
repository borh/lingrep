#!/usr/bin/env python3
"""Validate i18n key coverage for lingrep-web.

Contract:
- Every `data-i18n*` key referenced in templates (or via `t("…")` in JS) must
  exist in BOTH assets/i18n/en.json and assets/i18n/ja.json.
- Japanese values must be non-empty (an empty value silently leaves English —
  acceptable TEMPORARILY during rollout only).
- {placeholder} parity between en and ja per key.
"""
import json
import re
import sys
import pathlib

WEB = pathlib.Path(__file__).resolve().parent.parent / "lingrep-web"
TEMPLATES = (WEB / "src" / "templates").rglob("*.html")
JS = [
    WEB / "assets" / "results-explorer.js",
    WEB / "assets" / "app.js",
    WEB / "assets" / "i18n.js",
    WEB / "assets" / "query-library.js",
    # Relabels the editor size toggle, so it owns two keys of its own.
    WEB / "assets" / "editor-sizing.js",
]

KEY_ATTR = re.compile(r'data-i18n(?:-title|-placeholder|-aria-label)?="([a-z][\w.]*\w)"')
JS_TKEY = re.compile(r'\bt\("([a-z][\w.]*\w)"')
PLACEHOLDER = re.compile(r"\{(\w+)\}")
# Namespaced keys quoted anywhere in source (covers conditional t() calls and
# the i18n-key fields populated server-side in Rust).
NAMESPACED = re.compile(
    r'"((?:banner|capture|corpus|detail|editor|group|hero|help|hit|inspector|library|nav|notice|results|sample|status|token_inspection)\.[\w]+)"'
)
# Rust sources that name an i18n key directly: a key the server chooses (a
# landing notice, a refusal banner) is emitted through `data-i18n="{{ … }}"`,
# which no template scan can see.
RUST = [
    WEB / "src" / "revision_status.rs",
    WEB / "src" / "corpus_context.rs",
    WEB / "src" / "handlers" / "index.rs",
    WEB / "src" / "handlers" / "search.rs",
]


def template_keys():
    keys = set()
    for path in TEMPLATES:
        source = path.read_text()
        keys.update(KEY_ATTR.findall(source))
        # A key handed to a template macro is a reference too: the macro emits
        # `data-i18n="{{ … }}"`, which the attribute scan cannot see.
        keys.update(NAMESPACED.findall(source))
    for path in JS:
        if path.exists():
            keys.update(JS_TKEY.findall(path.read_text()))
            keys.update(NAMESPACED.findall(path.read_text()))
    for path in RUST:
        # Rust string literals escape their quotes, so unescaping first lets the
        # same attribute scan the templates get see an inline `data-i18n="…"`.
        source = path.read_text().replace('\\"', '"')
        keys.update(KEY_ATTR.findall(source))
        keys.update(NAMESPACED.findall(source))
    return keys


def main():
    en = json.loads((WEB / "assets/i18n/en.json").read_text())
    ja = json.loads((WEB / "assets/i18n/ja.json").read_text())
    used = template_keys()

    failures = []
    for key in sorted(used):
        if key not in en:
            failures.append(f"missing in en.json: {key}")
        if key not in ja:
            failures.append(f"missing in ja.json: {key}")
    for key in sorted(used):
        if key in en and key in ja:
            ep, jp = set(PLACEHOLDER.findall(en[key])), set(PLACEHOLDER.findall(ja[key]))
            if ep != jp:
                failures.append(f"placeholder mismatch {key}: en {sorted(ep)} ja {sorted(jp)}")
        if key in ja and ja[key].strip() == "":
            failures.append(f"empty Japanese value: {key}")
    # The dictionaries must not carry dead keys: dictionaries cover exactly
    # the used set (allow the toggle's fixed label, not expressed as a key).
    for key in sorted(set(en) | set(ja)):
        if key not in used and key != "locale.toggle_hint":
            failures.append(f"dictionary key never referenced: {key}")

    if failures:
        print("i18n key check failed:")
        for f in failures:
            print(" -", f)
        return 1
    print(f"i18n keys: {len(used)} referenced, both dictionaries consistent")
    return 0


if __name__ == "__main__":
    sys.exit(main())
