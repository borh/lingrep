import test from "node:test";
import assert from "node:assert/strict";

import {
  clearRecent,
  captureSubmission,
  corpusMismatch,
  createQueryStore,
  createCaptureQueue,
  databaseFailureReason,
  deleteEntry,
  enforceRetention,
  entryLabel,
  fingerprintSource,
  isEdited,
  loadCatalog,
  mergeSubmission,
  nextTab,
  normalizeName,
  queryControlState,
  recentEntries,
  removeFromRecent,
  restoreSource,
  saveSource,
  savedEntries,
  sourcePreview,
  unsaveEntry,
} from "../assets/query-library.js";

test("empty Recent disables clear and never exposes Show more", () => {
  assert.deepEqual(queryControlState("recent", 0, 0), {
    moreHidden: true,
    clearDisabled: true,
  });
});

test("Show more belongs only to a truncated Saved projection", () => {
  assert.deepEqual(queryControlState("saved", 20, 21), {
    moreHidden: false,
    clearDisabled: true,
  });
  assert.equal(queryControlState("saved", 21, 21).moreHidden, true);
});

const source = "first := [pos=NOUN].";

function entry(overrides = {}) {
  const sourceFingerprint = fingerprintSource(source);
  return {
    id: "q1",
    source,
    sourceFingerprint,
    dedupeKey: sourceFingerprint,
    firstDefinition: "first",
    name: null,
    submittedAt: null,
    savedAt: null,
    corpusDigest: null,
    ...overrides,
  };
}

test("fingerprint is deterministic, versioned, and source-sensitive", () => {
  assert.match(fingerprintSource("猫"), /^cyrb53-v1:[0-9a-f]+$/);
  assert.equal(fingerprintSource("猫"), fingerprintSource("猫"));
  assert.notEqual(fingerprintSource("猫"), fingerprintSource("犬"));
});

test("names trim by Unicode code point and never truncate", () => {
  assert.deepEqual(normalizeName("  research  "), {
    ok: true,
    value: "research",
  });
  assert.deepEqual(normalizeName(" \n "), { ok: true, value: null });
  assert.equal(normalizeName("猫".repeat(120)).ok, true);
  assert.deepEqual(normalizeName("🐈".repeat(121)), {
    ok: false,
    message: "Names must be 120 characters or fewer.",
  });
});

test("labels and previews use value fallbacks and Unicode-safe caps", () => {
  assert.equal(entryLabel(entry({ name: "Named" })), "Named");
  assert.equal(entryLabel(entry()), "first");
  assert.equal(entryLabel(entry({ firstDefinition: null })), "Untitled pattern");
  assert.equal([...sourcePreview("猫".repeat(200))].length, 161);
  assert.match(sourcePreview("猫".repeat(200)), /…$/u);
});

test("submission deduplicates exact source and preserves saved fields", () => {
  const existing = entry({
    name: "Keep",
    savedAt: "2026-07-23T01:00:00.000Z",
  });
  const merged = mergeSubmission(
    [existing],
    {
      source: existing.source,
      sourceFingerprint: existing.sourceFingerprint,
      firstDefinition: "first",
      submittedAt: "2026-07-23T02:00:00.000Z",
      corpusDigest: "abc",
    },
    "unused",
  );
  assert.equal(merged.length, 1);
  assert.deepEqual(merged[0], {
    ...existing,
    submittedAt: "2026-07-23T02:00:00.000Z",
    corpusDigest: "abc",
  });
});

test("same fingerprint with different source gets a collision key", () => {
  const existing = entry();
  const merged = mergeSubmission(
    [existing],
    {
      source: "other := [pos=VERB].",
      sourceFingerprint: existing.sourceFingerprint,
      firstDefinition: "other",
      submittedAt: "2026-07-23T03:00:00.000Z",
      corpusDigest: null,
    },
    "q2",
  );
  assert.equal(merged[1].dedupeKey, `${existing.sourceFingerprint}:q2`);
});

test("recent projection sorts descending", () => {
  const values = Array.from({ length: 3 }, (_, index) =>
    entry({
      id: `q${index}`,
      submittedAt: new Date(index * 1000).toISOString(),
    }),
  );
  assert.deepEqual(
    recentEntries(values).map((value) => value.id),
    ["q2", "q1", "q0"],
  );
});

test("retention caps Recent at 20 and preserves Saved ownership", () => {
  const values = Array.from({ length: 22 }, (_, index) =>
    entry({
      id: `q${index}`,
      source: `p${index} := [pos=NOUN].`,
      submittedAt: new Date(index * 1000).toISOString(),
      savedAt: index === 0 ? new Date(0).toISOString() : null,
    }),
  );
  const result = enforceRetention(values);
  assert.equal(recentEntries(result.entries).length, 20);
  assert.ok(result.entries.some((value) => value.id === "q0"));
  assert.equal(
    result.entries.find((value) => value.id === "q0").submittedAt,
    null,
  );
});

test("save and destructive transitions respect shared projection ownership", () => {
  const saved = saveSource(
    [],
    {
      source: "x := [pos=NOUN].",
      sourceFingerprint: fingerprintSource("x := [pos=NOUN]."),
      firstDefinition: "x",
      name: "X",
      savedAt: "2026-07-23T04:00:00.000Z",
    },
    "q2",
  );
  assert.equal(savedEntries(saved)[0].name, "X");

  const submitted = [
    { ...saved[0], submittedAt: "2026-07-23T05:00:00.000Z" },
  ];
  const removed = removeFromRecent(submitted, "q2");
  assert.equal(removed.entries[0].savedAt, saved[0].savedAt);
  assert.equal(removed.entries[0].submittedAt, null);
  assert.equal(removed.removed.length, 1);

  const cleared = clearRecent(submitted);
  assert.equal(cleared.entries[0].savedAt, saved[0].savedAt);
  assert.equal(cleared.entries[0].submittedAt, null);
  assert.equal(cleared.removed.length, 1);
  assert.equal(deleteEntry(submitted, "q2").entries.length, 0);
});

test("unsave clears a stale name while retaining a Recent entry", () => {
  const result = unsaveEntry(
    [
      entry({
        name: "Old saved name",
        savedAt: "2026-07-23T04:00:00.000Z",
        submittedAt: "2026-07-23T05:00:00.000Z",
      }),
    ],
    "q1",
  );
  assert.equal(result.entries.length, 1);
  assert.equal(result.entries[0].savedAt, null);
  assert.equal(result.entries[0].name, null);
  assert.equal(result.entries[0].submittedAt, "2026-07-23T05:00:00.000Z");
});

test("dirty and mismatch states require exact, known differences", () => {
  assert.equal(isEdited("a", "a"), false);
  assert.equal(isEdited("a", "b"), true);
  assert.equal(corpusMismatch("abc", "def"), true);
  assert.equal(corpusMismatch(null, "def"), false);
  assert.equal(corpusMismatch("abc", null), false);
});

test("restore writes the textarea and dispatches one bubbling input event", () => {
  let seen;
  const textarea = {
    value: "",
    dispatchEvent(event) {
      seen = event;
      return true;
    },
  };
  restoreSource(
    textarea,
    "x := [pos=NOUN].",
    (type, options) => ({ type, ...options }),
  );
  assert.equal(textarea.value, "x := [pos=NOUN].");
  assert.deepEqual(seen, { type: "input", bubbles: true });
});

test("tab arrows cycle over the three supported tabs in both directions", () => {
  assert.equal(nextTab("recent", "ArrowRight"), "saved");
  assert.equal(nextTab("saved", "ArrowRight"), "catalogs");
  assert.equal(nextTab("catalogs", "ArrowRight"), "recent");
  assert.equal(nextTab("recent", "ArrowLeft"), "catalogs");
  assert.equal(nextTab("catalogs", "ArrowLeft"), "saved");
  assert.equal(nextTab("recent", "Enter"), "recent");
});

/// The catalogs tab needs no change to this function: it lists a fixed set, so
/// paging and clearing are both inapplicable and the existing predicates
/// already yield that. Asserted rather than assumed.
test("the catalogs tab hides Show more and disables Clear", () => {
  assert.deepEqual(queryControlState("catalogs", 0, 11), {
    moreHidden: true,
    clearDisabled: true,
  });
});

test("loadCatalog returns source on success", async () => {
  const fetchImpl = async (url) => {
    assert.equal(url, "api/catalogs/sub/nested.lx");
    return { ok: true, text: async () => "nested := [].\n" };
  };
  assert.deepEqual(await loadCatalog("sub/nested.lx", fetchImpl), {
    ok: true,
    source: "nested := [].\n",
  });
});

test("loadCatalog reports a failed fetch without throwing", async () => {
  const fetchImpl = async () => ({ ok: false, status: 404, text: async () => "" });
  const result = await loadCatalog("absent.lx", fetchImpl);
  assert.equal(result.ok, false);
  assert.ok(result.message.length > 0);
});

test("loadCatalog rejects a rejected fetch without throwing", async () => {
  const result = await loadCatalog("x.lx", async () => {
    throw new Error("offline");
  });
  assert.equal(result.ok, false);
});

test("loadCatalog encodes each segment but keeps the separators", async () => {
  let seen = null;
  await loadCatalog("サブ/パターン.lx", async (url) => {
    seen = url;
    return { ok: true, text: async () => "" };
  });
  assert.equal(
    seen,
    "api/catalogs/%E3%82%B5%E3%83%96/%E3%83%91%E3%82%BF%E3%83%BC%E3%83%B3.lx",
  );
});

function memoryAdapter(seed = []) {
  let values = structuredClone(seed);
  return {
    async readAll() {
      return structuredClone(values);
    },
    async transact(transform) {
      const result = transform(structuredClone(values));
      values = structuredClone(result.entries);
      return structuredClone(result.value);
    },
    snapshot() {
      return structuredClone(values);
    },
  };
}

test("store atomically records a submission and preserves saved fields", async () => {
  const adapter = memoryAdapter([
    entry({
      name: "Saved name",
      savedAt: "2026-07-23T00:00:00.000Z",
    }),
  ]);
  const store = createQueryStore(adapter, { uuid: () => "new-id" });
  const result = await store.recordSubmission({
    source,
    submittedAt: "2026-07-23T06:00:00.000Z",
    corpusDigest: "abc",
  });
  assert.equal(result.ok, true);
  assert.equal(adapter.snapshot()[0].name, "Saved name");
  assert.equal(
    adapter.snapshot()[0].submittedAt,
    "2026-07-23T06:00:00.000Z",
  );
});

test("store maps transaction errors to unavailable without throwing", async () => {
  const adapter = {
    async readAll() {
      throw new DOMException("blocked", "QuotaExceededError");
    },
    async transact() {
      throw new Error("must not write");
    },
  };
  const store = createQueryStore(adapter, { uuid: () => "q" });
  assert.deepEqual(await store.list(), {
    ok: false,
    reason: "unavailable",
  });
  assert.deepEqual(
    await store.recordSubmission({
      source,
      submittedAt: "2026-07-23T06:00:00.000Z",
      corpusDigest: null,
    }),
    { ok: false, reason: "unavailable" },
  );
});

test("store retention deletes only entries belonging to neither projection", async () => {
  const values = Array.from({ length: 21 }, (_, index) =>
    entry({
      id: `q${index}`,
      source: `p${index} := [pos=NOUN].`,
      submittedAt: new Date(index * 1000).toISOString(),
      savedAt: index === 0 ? new Date(0).toISOString() : null,
    }),
  );
  const adapter = memoryAdapter(values);
  const store = createQueryStore(adapter, { uuid: () => "new-id" });
  await store.recordSubmission({
    source: "new := [pos=VERB].",
    submittedAt: "2026-07-23T07:00:00.000Z",
    corpusDigest: null,
  });
  assert.ok(adapter.snapshot().some((value) => value.id === "q0"));
  assert.equal(
    adapter.snapshot().filter((value) => value.submittedAt).length,
    20,
  );
});

test("store validates malformed records and reports one warning", async () => {
  const adapter = memoryAdapter([entry(), { id: "broken", source: 42 }]);
  const result = await createQueryStore(adapter).list();
  assert.equal(result.ok, true);
  assert.equal(result.value.length, 1);
  assert.equal(result.warning, "Some saved queries could not be read.");
});

test("store rejects malformed timestamps before rendering", async () => {
  const malformed = entry({ submittedAt: "not-a-date" });
  const result = await createQueryStore(memoryAdapter([malformed])).list();
  assert.equal(result.ok, true);
  assert.deepEqual(result.value, []);
  assert.equal(result.warning, "Some saved queries could not be read.");
});

test("newer IndexedDB schemas are classified without destructive recovery", () => {
  assert.equal(
    databaseFailureReason(new DOMException("newer", "VersionError")),
    "unsupported-schema",
  );
  assert.equal(
    databaseFailureReason(new DOMException("blocked", "QuotaExceededError")),
    "unavailable",
  );
});

test("submission timestamp is captured before persistence is scheduled", async () => {
  const scheduled = [];
  const persisted = [];
  const capture = captureSubmission({
    source,
    corpusDigest: "abc",
    now: () => 1234,
    schedule: (callback) => scheduled.push(callback),
    persist: async (value) => persisted.push(value),
  });
  assert.equal(capture.submittedAt, "1970-01-01T00:00:01.234Z");
  assert.equal(scheduled.length, 1);
  assert.equal(persisted.length, 0);
  await scheduled[0]();
  assert.deepEqual(persisted, [capture]);
});

test("capture queue flushes submissions released before storage is ready", async () => {
  const queue = createCaptureQueue();
  const seen = [];
  queue.enqueue({ source: "a" });
  queue.enqueue({ source: "b" });
  assert.equal(queue.pending(), 2);
  await queue.attach(async (capture) => seen.push(capture.source));
  assert.deepEqual(seen, ["a", "b"]);
  assert.equal(queue.pending(), 0);
  await queue.enqueue({ source: "c" });
  assert.deepEqual(seen, ["a", "b", "c"]);
});
