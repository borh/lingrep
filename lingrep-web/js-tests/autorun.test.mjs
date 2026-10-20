import assert from "node:assert/strict";
import test from "node:test";
import {
  shouldAutoRunInlineSample,
  shouldRunOnReady,
  stickyOffset,
} from "../assets/app.js";

// A landing on a cold corpus seeds the pattern without running it, and marks the
// form `data-run-when-ready`. The corpus-context poll is the only thing that
// learns the corpus became Ready, so the guard below is what turns that poll into
// exactly one deferred submit.

const form = (attrs = {}) => ({
  hasAttribute: (name) => name in attrs,
  removeAttribute: () => {},
});
const context = (digest) => ({
  getAttribute: (name) => (name === "data-corpus-digest" ? digest : null),
});

test("runs once when a pending form meets a ready context", () => {
  assert.equal(
    shouldRunOnReady(form({ "data-run-when-ready": "" }), context("abcdef012345")),
    true,
  );
});

test("does not run while the corpus is still preparing", () => {
  assert.equal(
    shouldRunOnReady(form({ "data-run-when-ready": "" }), context(null)),
    false,
  );
});

test("does not run when no rerun is pending", () => {
  assert.equal(shouldRunOnReady(form({}), context("abcdef012345")), false);
});

test("tolerates a missing form or context", () => {
  assert.equal(shouldRunOnReady(null, context("abcdef012345")), false);
  assert.equal(
    shouldRunOnReady(form({ "data-run-when-ready": "" }), null),
    false,
  );
});

test("the sticky offset is the band's height only while the band is pinned", () => {
  // Below the breakpoint the band scrolls away with the page, so the pattern
  // rail and the inspector dock must keep the offsets they had before it
  // existed — publishing a height there would push them down for nothing.
  assert.equal(stickyOffset(148, false), 0);
  assert.equal(stickyOffset(148.4, true), 148);
  assert.equal(stickyOffset(0, true), 0);
  assert.equal(stickyOffset(Number.NaN, true), 0);
});

test("inline sample auto-run fires only while the drawer is open and sample text is non-empty", () => {
  assert.equal(shouldAutoRunInlineSample(false, "猫が走る。"), true);
  assert.equal(shouldAutoRunInlineSample(true, "猫が走る。"), false);
  assert.equal(shouldAutoRunInlineSample(false, "   "), false);
  assert.equal(shouldAutoRunInlineSample(false, ""), false);
  assert.equal(shouldAutoRunInlineSample(false, null), false);
});
