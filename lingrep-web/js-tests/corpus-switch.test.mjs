import assert from "node:assert/strict";
import test from "node:test";
import {
  corpusSwitchOutcome,
  corpusSwitchRequestBody,
  permalinkRequestBody,
  permalinkRequestForForm,
} from "../assets/app.js";

// Switching corpora navigates to a permalink the server minted for the newly
// selected key. Every answer that is not a usable path must report instead:
// navigating to `undefined` is the one outcome this decision exists to prevent.

test("a minted query is navigated to without escaping the current mount", () => {
  assert.deepEqual(corpusSwitchOutcome(200, { path: "?v=2&q=AAAA&k=sci" }), {
    action: "navigate",
    path: "?v=2&q=AAAA&k=sci",
  });
});

test("an unknown corpus reports rather than navigating", () => {
  const outcome = corpusSwitchOutcome(404, { error: "unknown_corpus" });
  assert.equal(outcome.action, "report");
  assert.equal(outcome.key, "corpus.switch_unknown");
  assert.equal(outcome.path, undefined);
});

test("a too-large pattern reports rather than navigating", () => {
  const outcome = corpusSwitchOutcome(413, { error: "too_large" });
  assert.equal(outcome.action, "report");
  assert.equal(outcome.key, "corpus.switch_too_large");
  assert.equal(outcome.path, undefined);
});

test("any other status reports", () => {
  for (const status of [500, 503, 0]) {
    const outcome = corpusSwitchOutcome(status, null);
    assert.equal(outcome.action, "report");
    assert.equal(outcome.key, "corpus.switch_failed");
  }
});

test("a 200 with no usable path reports rather than navigating", () => {
  for (const payload of [null, {}, { path: 12 }, { path: "https://evil.example/" }, { path: "/help" }]) {
    const outcome = corpusSwitchOutcome(200, payload);
    assert.equal(outcome.action, "report", JSON.stringify(payload));
    assert.equal(outcome.key, "corpus.switch_failed");
    assert.equal(outcome.path, undefined);
  }
});

test("every reported outcome carries English text alongside its key", () => {
  for (const status of [404, 413, 500]) {
    const outcome = corpusSwitchOutcome(status, null);
    assert.equal(typeof outcome.text, "string");
    assert.ok(outcome.text.length > 0);
  }
});

// The permalink body is shared by the copy button and the switch, so the corpus
// a link targets is the corpus the page is on — never the server's default.

test("the request body carries the named corpus", () => {
  assert.equal(
    permalinkRequestBody("p := [].", "sci"),
    "pattern=p+%3A%3D+%5B%5D.&corpus=sci",
  );
});

test("a page naming no corpus omits the field, leaving the server's default", () => {
  for (const corpus of [undefined, null, ""]) {
    assert.equal(permalinkRequestBody("p := [].", corpus), "pattern=p+%3A%3D+%5B%5D.");
  }
});

// The Copy link button's own decision, not just the body helper it calls: this
// is the call site that reads the corpus off the form, and dropping that read is
// exactly the defect the corpus argument was added to fix.

const form = (fields) => ({ querySelector: (selector) => fields[selector] ?? null });
const searchForm = (pattern, corpus, retained) =>
  form({
    "#corpus-pattern": { value: pattern },
    "input[name=corpus]": { value: corpus },
    ...(retained === undefined ? {} : { "select[name=retained]": { value: retained } }),
  });

test("the request body carries the page's retention when it has one", () => {
  assert.equal(
    permalinkRequestBody("p := [].", "sci", "50"),
    "pattern=p+%3A%3D+%5B%5D.&corpus=sci&retained=50",
  );
  for (const retained of [undefined, null, ""]) {
    assert.equal(permalinkRequestBody("p := [].", "sci", retained), "pattern=p+%3A%3D+%5B%5D.&corpus=sci");
  }
  assert.equal(
    permalinkRequestForForm(searchForm("p := [].", "sci", "50")).body,
    "pattern=p+%3A%3D+%5B%5D.&corpus=sci&retained=50",
  );
});

test("copying a link names the corpus the page is pointed at", () => {
  const request = permalinkRequestForForm(searchForm("p := [].", "sci"));
  assert.equal(request.action, "request");
  assert.equal(request.body, "pattern=p+%3A%3D+%5B%5D.&corpus=sci");
  assert.ok(
    request.body.includes("corpus=sci"),
    "the copied link must target the page's corpus, not the server's default",
  );
});

test("a page pointed at no corpus refuses to mint a link", () => {
  // An unknown-corpus landing: the hidden field is empty, so minting would
  // silently hand back a link to the server's default corpus instead.
  for (const empty of ["", undefined]) {
    const request = permalinkRequestForForm(searchForm("p := [].", empty));
    assert.equal(request.action, "report");
    assert.equal(request.key, "corpus.select_before_link");
    assert.equal(request.body, undefined);
  }
});

test("a form with no fields at all refuses rather than minting", () => {
  assert.equal(permalinkRequestForForm(form({})).action, "report");
  assert.equal(permalinkRequestForForm(null).action, "report");
});


test("copying a link preserves the form's section selection and unlabelled preference", () => {
  const scope = JSON.stringify({ included_sections: ["introduction", "methods"], include_unlabelled: false });
  const request = permalinkRequestForForm(form({
    "#corpus-pattern": { value: "p := []." },
    "input[name=corpus]": { value: "sci" },
    "select[name=retained]": { value: "50" },
    "[data-search-scope-value]": { value: scope },
  }));
  assert.equal(request.action, "request");
  assert.equal(new URLSearchParams(request.body).get("search_scope"), scope);
  assert.equal(new URLSearchParams(permalinkRequestBody("p := [].", "other", "50", scope)).get("search_scope"), scope);
});

test("switching corpora discards the previous vocabulary and asks for the destination default", () => {
  const fields = form({
    "#corpus-pattern": { value: "p := []." },
    "select[name=retained]": { value: "50" },
    "[data-search-scope-value]": { value: JSON.stringify({ included_sections: ["methods"], include_unlabelled: false }) },
  });
  const body = new URLSearchParams(corpusSwitchRequestBody(fields, "bccwj"));
  assert.equal(body.get("corpus"), "bccwj");
  assert.equal(body.get("pattern"), "p := [].");
  assert.equal(body.get("retained"), "50");
  assert.equal(body.has("search_scope"), false);
});

test("copying before a cold corpus supplies its vocabulary retains the destination default", () => {
  assert.equal(new URLSearchParams(permalinkRequestBody("p := [].", "hssrpc", "50", "null")).has("search_scope"), false);
});
