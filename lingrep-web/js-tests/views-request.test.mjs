import test from "node:test";
import assert from "node:assert/strict";
import { api, latestRequest, loadMeta, pager, redrawKeepingFocus, showCorpus } from "../assets/views.js";

function answer(reply) {
  const calls = [];
  globalThis.fetch = async (url, init) => {
    calls.push({ url, signal: init.signal });
    return reply;
  };
  return calls;
}

test("a request leaves out empty parameters and returns the JSON reply", async () => {
  const calls = answer({ ok: true, json: async () => ({ status: "ok" }) });
  const signal = new AbortController().signal;
  assert.deepEqual(await api("frequency", { corpus: "a b", w: undefined, where: null, q: "", offset: 0 }, signal), { status: "ok" });
  assert.deepEqual(calls, [{ url: "api/frequency?corpus=a+b&offset=0", signal }]);
});

test("a failed status rejects with the reply's error", async () => {
  answer({ ok: false, status: 400, json: async () => ({ error: "no pattern “x”" }) });
  await assert.rejects(api("frequency", {}), { message: "no pattern “x”" });
});

test("a failed status without a JSON body rejects with the status", async () => {
  answer({ ok: false, status: 500, json: async () => JSON.parse("") });
  await assert.rejects(api("frequency", {}), { message: "Server response: 500." });
});

test("a reply that is not JSON rejects", async () => {
  answer({ ok: true, json: async () => JSON.parse("<html>") });
  await assert.rejects(api("frequency", {}), SyntaxError);
});

test("requesting a new signal aborts only the previous signal", () => {
  const requests = latestRequest();
  requests.abort();
  const first = requests.next();
  assert.equal(first.aborted, false);
  const second = requests.next();
  assert.equal(first.aborted, true);
  assert.equal(second.aborted, false);
  requests.abort();
  assert.equal(second.aborted, true);
});

function held() {
  let settle;
  const promise = new Promise((resolve, reject) => (settle = { resolve, reject }));
  return { promise, ...settle };
}

test("receiving a page clears the pending state and redraws the button", async () => {
  const events = [], reply = held();
  const paging = pager((message) => events.push(["report", message]));
  const done = paging.next(() => reply.promise, () => true, (page) => events.push(["receive", page.rows]), () => events.push(["redraw", paging.pending]));
  assert.equal(paging.pending, true);
  reply.resolve({ status: "ok", rows: [1] });
  await done;
  assert.deepEqual(events, [["receive", [1]], ["redraw", false]]);
});

test("a page that arrives for a replaced answer is dropped without a message", async () => {
  const events = [];
  const paging = pager((message) => events.push(["report", message]));
  await paging.next(async () => ({ status: "ok", rows: [1] }), () => false, () => events.push(["receive"]), () => events.push(["redraw"]));
  await paging.next(async () => { throw new Error("late failure"); }, () => false, () => events.push(["receive"]), () => events.push(["redraw"]));
  assert.deepEqual(events, [["redraw"], ["redraw"]]);
});

test("a failed page reports its message; an unsupported reply and an aborted request report nothing", async () => {
  const events = [];
  const paging = pager((message) => events.push(message));
  const redraw = () => events.push("redraw");
  await paging.next(async () => { throw new Error("no table"); }, () => true, () => events.push("receive"), redraw);
  await paging.next(async () => ({ status: "unsupported" }), () => true, () => events.push("receive"), redraw);
  const done = paging.next((signal) => new Promise((_, reject) => signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")))), () => true, () => events.push("receive"), redraw);
  paging.cancel();
  await done;
  assert.deepEqual(events, ["no table", "redraw", "redraw", "redraw"]);
  assert.equal(paging.pending, false);
});

test("failed metadata leaves its message in the state line", async () => {
  const state = { textContent: "" };
  answer({ ok: false, status: 500, json: async () => ({ error: "no metadata" }) });
  assert.equal(await loadMeta(state), null);
  assert.equal(state.textContent, "no metadata");
  answer({ ok: true, json: async () => ({ pairs: [] }) });
  assert.deepEqual(await loadMeta(state), { pairs: [] });
});

test("a redraw returns focus and the caret to the control that held them", () => {
  const drawn = [];
  const control = (name) => ({ name, focus: (options) => drawn.push(["focus", name, options]), setSelectionRange: (...caret) => drawn.push(["caret", ...caret]) });
  const input = { ...control("q"), selectionStart: 2, selectionEnd: 3, selectionDirection: "forward" };
  const container = { contains: (element) => element === input, querySelector: (selector) => control(selector) };
  globalThis.document = { activeElement: input };
  redrawKeepingFocus(container, (element) => `[name=${element.name}]`, () => drawn.push(["draw"]));
  assert.deepEqual(drawn, [["draw"], ["focus", "[name=q]", { preventScroll: true }], ["caret", 2, 3, "forward"]]);

  drawn.length = 0;
  globalThis.document = { activeElement: {} };
  redrawKeepingFocus(container, () => "[name=other]", () => drawn.push(["draw"]));
  assert.deepEqual(drawn, [["draw"]]);
});

test("the corpus line shows the corpus's display name, or the key of a corpus without tables", () => {
  const name = { textContent: "" }, line = { hidden: true, querySelector: () => name };
  const root = { querySelector: () => line };
  const meta = { pairs: [{ corpus: "default", corpus_name: "everyday-corpus" }] };
  showCorpus(root, meta, "default");
  assert.deepEqual([name.textContent, line.hidden], ["everyday-corpus", false]);
  showCorpus(root, meta, "elsewhere");
  assert.deepEqual([name.textContent, line.hidden], ["elsewhere", false]);
  showCorpus(root, { pairs: [] }, null);
  assert.deepEqual([name.textContent, line.hidden], ["", true]);
});
