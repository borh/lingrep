import assert from "node:assert/strict";
import test from "node:test";
import { isFinishedSseEvent } from "../assets/results-explorer.js";

// A terminal SSE frame must suppress app.js's "connection lost" notice so a
// failed/finished scan shows one message, not two stacked contradictions.
test("terminal SSE event names finish the stream guard", () => {
  assert.equal(isFinishedSseEvent("result"), true);
  assert.equal(isFinishedSseEvent("error"), true);
  assert.equal(isFinishedSseEvent("progress"), false);
  assert.equal(isFinishedSseEvent(undefined), false);
  assert.equal(isFinishedSseEvent(""), false);
});
