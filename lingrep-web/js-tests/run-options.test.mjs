import assert from "node:assert/strict";
import test from "node:test";
import { runOptionsSummary } from "../assets/app.js";

const translate = (key, params) => (params ? `${key}(${params.n})` : key);

test("the closed run options state the hits kept and each option that changes the run", () => {
  assert.equal(
    runOptionsSummary({ retained: "1000", deriveCompounds: false, documentReach: false, overlaps: "catalog" }, translate),
    "editor.run_options_retained(1000)",
  );
  assert.equal(
    runOptionsSummary({ retained: "50", deriveCompounds: true, documentReach: true, overlaps: "longest" }, translate),
    "editor.run_options_retained(50) · editor.derive_compounds · editor.document_reach · editor.overlaps_longest",
  );
  assert.equal(
    runOptionsSummary({ retained: "50", deriveCompounds: false, documentReach: false, overlaps: "keep" }, translate),
    "editor.run_options_retained(50) · editor.overlaps_keep",
  );
});
