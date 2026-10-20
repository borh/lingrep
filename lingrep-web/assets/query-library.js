import { focusEditor, patternDefinitions } from "./editor/pattern-editor.js";
import { t, onLocaleChange } from "./i18n.js";

export const RECENT_LIMIT = 20;
export const SAVED_PAGE_SIZE = 50;

const NAME_LIMIT = 120;
const PREVIEW_LIMIT = 160;

export function fingerprintSource(source, seed = 0) {
  let h1 = 0xdeadbeef ^ seed;
  let h2 = 0x41c6ce57 ^ seed;
  for (let index = 0; index < source.length; index++) {
    const code = source.charCodeAt(index);
    h1 = Math.imul(h1 ^ code, 2654435761);
    h2 = Math.imul(h2 ^ code, 1597334677);
  }
  h1 =
    Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^
    Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 =
    Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^
    Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  const value = 4294967296 * (2097151 & h2) + (h1 >>> 0);
  return `cyrb53-v1:${value.toString(16)}`;
}

export function normalizeName(value) {
  const trimmed = value.trim();
  if ([...trimmed].length > NAME_LIMIT) {
    return {
      ok: false,
      message: "Names must be 120 characters or fewer.",
    };
  }
  return { ok: true, value: trimmed || null };
}

export function firstDefinition(source) {
  return patternDefinitions(source)[0]?.name ?? null;
}

export function entryLabel(entry) {
  return entry.name || entry.firstDefinition || "Untitled pattern";
}

export function sourcePreview(source) {
  const points = [...source.replace(/\s+/gu, " ").trim()];
  return points.length <= PREVIEW_LIMIT
    ? points.join("")
    : `${points.slice(0, PREVIEW_LIMIT).join("")}…`;
}

export function corpusMismatch(recorded, live) {
  return Boolean(recorded && live && recorded !== live);
}

export function isEdited(current, anchor) {
  return current !== anchor;
}

function byNewest(field) {
  return (left, right) => right[field].localeCompare(left[field]);
}

export function recentEntries(entries) {
  return entries
    .filter((entry) => entry.submittedAt !== null)
    .toSorted(byNewest("submittedAt"));
}

export function savedEntries(entries) {
  return entries
    .filter((entry) => entry.savedAt !== null)
    .toSorted(byNewest("savedAt"));
}

export function queryControlState(projection, shownCount, totalCount) {
  return {
    moreHidden: projection !== "saved" || shownCount >= totalCount,
    clearDisabled: projection !== "recent" || totalCount === 0,
  };
}

function dedupeKey(entries, sourceFingerprint, source, id) {
  const bucket = entries.filter(
    (entry) => entry.sourceFingerprint === sourceFingerprint,
  );
  if (!bucket.length || bucket.some((entry) => entry.source === source)) {
    return sourceFingerprint;
  }
  return `${sourceFingerprint}:${id}`;
}

export function mergeSubmission(entries, capture, id) {
  const existing = entries.find((entry) => entry.source === capture.source);
  if (existing) {
    return entries.map((entry) =>
      entry.id === existing.id
        ? {
            ...entry,
            submittedAt: capture.submittedAt,
            corpusDigest: capture.corpusDigest,
          }
        : entry,
    );
  }
  return [
    ...entries,
    {
      id,
      source: capture.source,
      sourceFingerprint: capture.sourceFingerprint,
      dedupeKey: dedupeKey(
        entries,
        capture.sourceFingerprint,
        capture.source,
        id,
      ),
      firstDefinition: capture.firstDefinition,
      name: null,
      submittedAt: capture.submittedAt,
      savedAt: null,
      corpusDigest: capture.corpusDigest,
    },
  ];
}

export function saveSource(entries, input, id) {
  const existing = entries.find((entry) => entry.source === input.source);
  if (existing) {
    return entries.map((entry) =>
      entry.id === existing.id
        ? {
            ...entry,
            firstDefinition: input.firstDefinition,
            name: input.name,
            savedAt: input.savedAt,
          }
        : entry,
    );
  }
  return [
    ...entries,
    {
      id,
      source: input.source,
      sourceFingerprint: input.sourceFingerprint,
      dedupeKey: dedupeKey(
        entries,
        input.sourceFingerprint,
        input.source,
        id,
      ),
      firstDefinition: input.firstDefinition,
      name: input.name,
      submittedAt: null,
      savedAt: input.savedAt,
      corpusDigest: input.corpusDigest ?? null,
    },
  ];
}

function retainClaimed(entries) {
  return entries.filter(
    (entry) => entry.submittedAt !== null || entry.savedAt !== null,
  );
}

export function enforceRetention(entries) {
  const excess = recentEntries(entries).slice(RECENT_LIMIT);
  const excessIds = new Set(excess.map((entry) => entry.id));
  const updated = entries.map((entry) =>
    excessIds.has(entry.id) ? { ...entry, submittedAt: null } : entry,
  );
  return { entries: retainClaimed(updated), removed: excess };
}

export function removeFromRecent(entries, id) {
  const removed = entries.filter((entry) => entry.id === id);
  const updated = entries.map((entry) =>
    entry.id === id ? { ...entry, submittedAt: null } : entry,
  );
  return { entries: retainClaimed(updated), removed };
}

export function clearRecent(entries) {
  const removed = entries.filter((entry) => entry.submittedAt !== null);
  const updated = entries.map((entry) =>
    entry.submittedAt !== null ? { ...entry, submittedAt: null } : entry,
  );
  return { entries: retainClaimed(updated), removed };
}

export function unsaveEntry(entries, id) {
  const removed = entries.filter((entry) => entry.id === id);
  const updated = entries.map((entry) =>
    entry.id === id ? { ...entry, savedAt: null, name: null } : entry,
  );
  return { entries: retainClaimed(updated), removed };
}

export function deleteEntry(entries, id) {
  const removed = entries.filter((entry) => entry.id === id);
  return {
    entries: entries.filter((entry) => entry.id !== id),
    removed,
  };
}

export function restoreSource(
  textarea,
  source,
  eventFactory = (type, options) => new Event(type, options),
) {
  textarea.value = source;
  textarea.dispatchEvent(eventFactory("input", { bubbles: true }));
}

const TABS = ["recent", "saved", "catalogs"];

export function nextTab(current, key) {
  const step = key === "ArrowRight" ? 1 : key === "ArrowLeft" ? -1 : 0;
  if (step === 0) return current;
  const index = TABS.indexOf(current);
  if (index === -1) return current;
  return TABS[(index + step + TABS.length) % TABS.length];
}

/**
 * Fetch one catalog's lingrep patterns source.
 *
 * The server route is a single catch-all that captures the remainder of the
 * path; `Path<String>` percent-decodes it, and the decoded string is looked
 * up as an exact key against the catalog index. So each path segment is
 * encoded individually (for non-ASCII and reserved characters) while the
 * separators stay real slashes, so the captured, decoded path matches the
 * stored key.
 */
export async function loadCatalog(key, fetchImpl = fetch) {
  const path = key.split("/").map(encodeURIComponent).join("/");
  try {
    const response = await fetchImpl(`api/catalogs/${path}`);
    if (!response.ok) {
      return { ok: false, message: `Catalog unavailable (${response.status}).` };
    }
    return { ok: true, source: await response.text() };
  } catch (error) {
    return { ok: false, message: `Catalog unavailable (${error}).` };
  }
}

export function captureSubmission({
  source,
  corpusDigest,
  now,
  schedule,
  persist,
}) {
  const capture = {
    source,
    corpusDigest,
    submittedAt: new Date(now()).toISOString(),
  };
  schedule(() => persist(capture));
  return capture;
}

export function createCaptureQueue() {
  let persist = null;
  let pending = [];
  let tail = Promise.resolve();

  return {
    enqueue(capture) {
      if (!persist) {
        pending.push(capture);
        return Promise.resolve();
      }
      tail = tail.then(() => persist(capture));
      return tail;
    },
    attach(nextPersist) {
      persist = nextPersist;
      const queued = pending;
      pending = [];
      for (const capture of queued) {
        tail = tail.then(() => persist(capture));
      }
      return tail;
    },
    pending() {
      return pending.length;
    },
  };
}

function validNullableString(value) {
  return value === null || typeof value === "string";
}

function validTimestamp(value) {
  if (value === null) return true;
  if (typeof value !== "string") return false;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) && new Date(parsed).toISOString() === value;
}

export function validEntry(value) {
  return (
    value !== null &&
    typeof value === "object" &&
    typeof value.id === "string" &&
    typeof value.source === "string" &&
    typeof value.sourceFingerprint === "string" &&
    typeof value.dedupeKey === "string" &&
    validNullableString(value.firstDefinition) &&
    validNullableString(value.name) &&
    validTimestamp(value.submittedAt) &&
    validTimestamp(value.savedAt) &&
    validNullableString(value.corpusDigest)
  );
}

function unavailable() {
  return { ok: false, reason: "unavailable" };
}

export function databaseFailureReason(error) {
  return error?.name === "VersionError"
    ? "unsupported-schema"
    : "unavailable";
}

export function createQueryStore(
  adapter,
  { uuid = () => crypto.randomUUID() } = {},
) {
  async function list() {
    try {
      const values = await adapter.readAll();
      const valid = values.filter(validEntry);
      return {
        ok: true,
        value: valid,
        ...(valid.length === values.length
          ? {}
          : { warning: "Some saved queries could not be read." }),
      };
    } catch (_error) {
      return unavailable();
    }
  }

  async function mutate(transform) {
    try {
      const value = await adapter.transact((stored) => {
        const entries = stored.filter(validEntry);
        const result = transform(entries);
        return {
          entries: result.entries,
          value: result.value ?? result,
        };
      });
      return { ok: true, value };
    } catch (_error) {
      return unavailable();
    }
  }

  return {
    list,

    recordSubmission(capture) {
      const normalized = {
        ...capture,
        sourceFingerprint: fingerprintSource(capture.source),
        firstDefinition: firstDefinition(capture.source),
      };
      return mutate((entries) => {
        const merged = mergeSubmission(entries, normalized, uuid());
        const retained = enforceRetention(merged);
        return { entries: retained.entries, value: normalized };
      });
    },

    save(input) {
      const normalizedName = normalizeName(input.name ?? "");
      if (!normalizedName.ok) {
        return Promise.resolve(normalizedName);
      }
      const normalized = {
        source: input.source,
        sourceFingerprint: fingerprintSource(input.source),
        firstDefinition: firstDefinition(input.source),
        name: normalizedName.value,
        savedAt: input.savedAt,
        corpusDigest: input.corpusDigest ?? null,
      };
      return mutate((entries) => ({
        entries: saveSource(entries, normalized, uuid()),
        value: normalized,
      }));
    },

    rename(id, name) {
      const normalized = normalizeName(name);
      if (!normalized.ok) return Promise.resolve(normalized);
      return mutate((entries) => ({
        entries: entries.map((entry) =>
          entry.id === id ? { ...entry, name: normalized.value } : entry,
        ),
        value: entries.find((entry) => entry.id === id) ?? null,
      }));
    },

    removeRecent(id) {
      return mutate((entries) => removeFromRecent(entries, id));
    },

    clearRecent() {
      return mutate(clearRecent);
    },

    unsave(id) {
      return mutate((entries) => unsaveEntry(entries, id));
    },

    delete(id) {
      return mutate((entries) => deleteEntry(entries, id));
    },

    restore(records) {
      return mutate((entries) => {
        const restored = new Map(entries.map((entry) => [entry.id, entry]));
        for (const record of records.filter(validEntry)) {
          restored.set(record.id, record);
        }
        return { entries: [...restored.values()], value: records };
      });
    },
  };
}

const DATABASE_NAME = "lingrep-web";
const DATABASE_VERSION = 1;
const QUERY_STORE = "queries";

function requestPromise(request) {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

function transactionPromise(transaction) {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onabort = () =>
      reject(transaction.error ?? new DOMException("Transaction aborted"));
    transaction.onerror = () =>
      reject(transaction.error ?? new DOMException("Transaction failed"));
  });
}

export function openQueryDatabase(indexedDBFactory = globalThis.indexedDB) {
  if (!indexedDBFactory) {
    return Promise.reject(new DOMException("IndexedDB unavailable"));
  }
  return new Promise((resolve, reject) => {
    const request = indexedDBFactory.open(DATABASE_NAME, DATABASE_VERSION);
    let settled = false;
    request.onupgradeneeded = () => {
      const database = request.result;
      if (!database.objectStoreNames.contains(QUERY_STORE)) {
        const store = database.createObjectStore(QUERY_STORE, {
          keyPath: "id",
        });
        store.createIndex("sourceFingerprint", "sourceFingerprint", {
          unique: false,
        });
        store.createIndex("dedupeKey", "dedupeKey", { unique: true });
      }
    };
    request.onsuccess = () => {
      if (settled) {
        request.result.close();
        return;
      }
      settled = true;
      resolve(request.result);
    };
    request.onerror = () => {
      if (settled) return;
      settled = true;
      reject(request.error);
    };
    request.onblocked = () => {
      if (settled) return;
      settled = true;
      reject(new DOMException("IndexedDB upgrade blocked", "InvalidStateError"));
    };
  });
}

export function createIndexedDbAdapter(database) {
  return {
    async readAll() {
      const transaction = database.transaction(QUERY_STORE, "readonly");
      const values = await requestPromise(
        transaction.objectStore(QUERY_STORE).getAll(),
      );
      await transactionPromise(transaction);
      return values;
    },

    async transact(transform) {
      const transaction = database.transaction(QUERY_STORE, "readwrite");
      const store = transaction.objectStore(QUERY_STORE);
      const entries = await requestPromise(store.getAll());
      const result = transform(entries);
      await requestPromise(store.clear());
      for (const entry of result.entries) {
        await requestPromise(store.put(entry));
      }
      await transactionPromise(transaction);
      return result.value;
    },
  };
}

function liveCorpusDigest(root) {
  return (
    root
      .querySelector(".corpus-context[data-corpus-digest]")
      ?.getAttribute("data-corpus-digest") ?? null
  );
}

function button(label, action, id = null) {
  const control = document.createElement("button");
  control.type = "button";
  control.className = "btn btn-secondary";
  control.textContent = label;
  control.dataset.queryAction = action;
  if (id) control.dataset.queryId = id;
  return control;
}

function relativeTime(timestamp) {
  const seconds = Math.round((Date.parse(timestamp) - Date.now()) / 1000);
  const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  if (Math.abs(seconds) < 60) return formatter.format(seconds, "second");
  const minutes = Math.round(seconds / 60);
  if (Math.abs(minutes) < 60) return formatter.format(minutes, "minute");
  const hours = Math.round(minutes / 60);
  if (Math.abs(hours) < 24) return formatter.format(hours, "hour");
  return formatter.format(Math.round(hours / 24), "day");
}

function timeNode(timestamp) {
  const time = document.createElement("time");
  time.dateTime = timestamp;
  time.textContent = relativeTime(timestamp);
  time.title = new Date(timestamp).toLocaleString();
  return time;
}

function namedButton(label, action, entry) {
  const control = button(label, action, entry.id);
  control.setAttribute("aria-label", `${label} “${entryLabel(entry)}”`);
  return control;
}

function rowNode(entry, projection, digest) {
  const row = document.createElement("article");
  row.className = "query-row";
  row.dataset.queryId = entry.id;

  const heading = document.createElement("h3");
  heading.className = "query-row-name";
  heading.textContent = entryLabel(entry);
  row.append(heading);

  const preview = document.createElement("p");
  preview.className = "query-preview";
  preview.textContent = sourcePreview(entry.source);
  row.append(preview);

  const metadata = document.createElement("p");
  metadata.className = "query-row-meta";
  const timestamp =
    projection === "recent" ? entry.submittedAt : entry.savedAt;
  if (timestamp) metadata.append(timeNode(timestamp));
  if (corpusMismatch(entry.corpusDigest, digest)) {
    const mismatch = document.createElement("span");
    mismatch.className = "query-mismatch";
    mismatch.textContent = t("library.different_corpus");
    metadata.append(" · ", mismatch);
  }
  row.append(metadata);

  const actions = document.createElement("div");
  actions.className = "query-row-actions";
  actions.append(
    namedButton(t("library.restore"), "restore", entry),
    namedButton(t("library.restore_run"), "restore-run", entry),
  );
  if (projection === "recent" && entry.savedAt === null) {
    actions.append(namedButton(t("library.save_entry"), "save", entry));
  }
  if (entry.savedAt !== null) {
    actions.append(
      namedButton(t("library.rename"), "rename", entry),
      namedButton(t("library.unsave"), "unsave", entry),
    );
  }
  if (entry.submittedAt !== null) {
    actions.append(namedButton(t("library.remove_recent"), "remove-recent", entry));
  }
  actions.append(namedButton(t("library.delete"), "delete", entry));
  row.append(actions);
  return row;
}

function inlineNameForm(row, entry, onSubmit) {
  row.querySelector(".query-inline-form")?.remove();
  const form = document.createElement("form");
  form.className = "query-inline-form";
  const input = document.createElement("input");
  input.type = "text";
  input.value = entry?.name ?? "";
  input.maxLength = 240;
  input.setAttribute("aria-label", t("library.name_for", { name: entryLabel(entry) }));
  const submit = document.createElement("button");
  submit.type = "submit";
  submit.className = "btn btn-primary";
  submit.textContent = t("library.save_name");
  const cancel = button(t("library.cancel"), "cancel-inline");
  const error = document.createElement("span");
  error.className = "query-inline-error";
  error.setAttribute("role", "alert");
  form.append(input, submit, cancel, error);
  cancel.addEventListener("click", () => form.remove());
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const normalized = normalizeName(input.value);
    if (!normalized.ok) {
      error.textContent = normalized.message;
      return;
    }
    await onSubmit(normalized.value);
  });
  row.append(form);
  input.focus();
}

function mismatchConfirmation(row, entry, run) {
  row.querySelector(".query-confirm")?.remove();
  const confirmation = document.createElement("div");
  confirmation.className = "query-confirm";
  const message = document.createElement("span");
  message.textContent = t("library.confirm_mismatch");
  const proceed = namedButton(t("library.run_anyway"), "confirm-run", entry);
  const cancel = button(t("library.cancel"), "cancel-run");
  proceed.addEventListener("click", () => {
    confirmation.remove();
    run();
  });
  cancel.addEventListener("click", () => confirmation.remove());
  confirmation.append(message, proceed, cancel);
  row.append(confirmation);
  proceed.focus();
}

function initCatalogTab(root, textarea) {
  const list = root.querySelector("[data-catalog-list]");
  if (!list) return;
  list.addEventListener("click", async (event) => {
    const button = event.target.closest("button[data-catalog-key]");
    if (!button) return;
    const result = await loadCatalog(button.dataset.catalogKey);
    const status = root.querySelector("[data-query-status]");
    if (!result.ok) {
      if (status) status.textContent = result.message;
      return;
    }
    restoreSource(textarea, result.source);
    // `anchorSource` is intentionally left alone here (unlike `restore()`
    // below): a catalog load is not a saved query, and leaving the anchor at
    // its prior value is what keeps `[data-query-edited]` visible after the
    // load, which is the documented, tested behavior. Do not "fix" this.
    // A load must still be visible, matching `restore()`: unfold a folded
    // editor workspace (app.js listens for this) and move focus there.
    document.dispatchEvent(new CustomEvent("lingrep-web:query-restored"));
    focusEditor(textarea);
    if (status) status.textContent = "";
  });
}

export async function initializeQueryLibrary(root = document) {
  const panel = root.querySelector("[data-query-library]");
  const form =
    root.querySelector("form[data-corpus-search]") ??
    root.querySelector(".sample-form");
  const textarea =
    root.querySelector("#corpus-pattern") ??
    root.querySelector("#sample-pattern");
  if (!panel || !form || !textarea || panel.dataset.queryReady === "true") {
    return;
  }
  panel.dataset.queryReady = "true";

  const listNode = panel.querySelector("[data-query-list]");
  const catalogListNode = panel.querySelector("[data-catalog-list]");
  const actionsNode = panel.querySelector(".query-library-actions");
  const statusNode = panel.querySelector("[data-query-status]");
  const editedNode = panel.querySelector("[data-query-edited]");
  const moreButton = panel.querySelector("[data-query-more]");
  const clearRecentButton = panel.querySelector("[data-clear-recent]");
  const tabs = [...panel.querySelectorAll("[data-query-tab]")];
  let projection = "recent";
  let savedLimit = SAVED_PAGE_SIZE;
  let anchorSource = textarea.value;
  let undoRecords = null;
  let panelInteractionAt = 0;
  const captureQueue = createCaptureQueue();
  let store;

  function status(message, undo = false) {
    statusNode.replaceChildren();
    if (message) statusNode.append(document.createTextNode(message));
    if (undo && undoRecords?.length) {
      statusNode.append(" ", button(t("library.undo"), "undo"));
    }
  }

  function renderEdited() {
    editedNode.hidden = !isEdited(textarea.value, anchorSource);
  }

  function restorePanelFocus(request) {
    if (!request) return;
    const target = request.selector
      ? panel.querySelector(request.selector)
      : panel.querySelector(
          `[data-query-id="${CSS.escape(request.id)}"] [data-query-action="${request.action}"]`,
        );
    const fallback = statusNode;
    fallback.tabIndex = -1;
    (target && !target.disabled ? target : fallback).focus();
  }

  async function render(force = false, focusRequest = null) {
    if (
      !force &&
      panel.querySelector(".query-inline-form, .query-confirm")
    ) {
      return;
    }
    const result = await store.list();
    if (!result.ok) {
      status(
        result.reason === "unsupported-schema"
          ? t("library.unsupported_schema")
          : t("library.unavailable"),
      );
      return;
    }
    if (
      !force &&
      panel.querySelector(".query-inline-form, .query-confirm")
    ) {
      return;
    }
    if (result.warning) status(result.warning);
    const all =
      projection === "recent"
        ? recentEntries(result.value)
        : savedEntries(result.value);
    const shown =
      projection === "saved" ? all.slice(0, savedLimit) : all.slice(0, 20);
    listNode.replaceChildren();
    if (!shown.length) {
      const empty = document.createElement("p");
      empty.className = "muted";
      empty.textContent =
        projection === "recent"
          ? t("library.no_recent")
          : t("library.no_saved");
      listNode.append(empty);
    } else {
      const digest = liveCorpusDigest(root);
      for (const entry of shown) {
        listNode.append(rowNode(entry, projection, digest));
      }
    }
    const controls = queryControlState(projection, shown.length, all.length);
    moreButton.hidden = controls.moreHidden;
    clearRecentButton.disabled = controls.clearDisabled;
    renderEdited();
    restorePanelFocus(focusRequest);
  }

  // Attach at once so a request released while IndexedDB opens is queued, not
  // silently omitted from Recent. Persistence remains off the request path.
  form.addEventListener("htmx:beforeRequest", (event) => {
    if (event.detail?.elt !== form) return;
    const capture = captureSubmission({
      source: textarea.value,
      corpusDigest: liveCorpusDigest(root),
      now: Date.now,
      schedule: queueMicrotask,
      persist: (captured) => captureQueue.enqueue(captured),
    });
    anchorSource = capture.source;
    renderEdited();
  });

  // Catalogs are server-rendered and need no database, so this tab and its
  // wiring are installed unconditionally, above the IndexedDB open below.
  // Recent/Saved still depend on `store`, so their render() calls below are
  // guarded on it being present; with no database, Catalogs still switches
  // panels and loads an entry, while Recent/Saved show their existing
  // unavailable-storage state instead of throwing.
  initCatalogTab(root, textarea);

  for (const tab of tabs) {
    tab.addEventListener("click", async () => {
      projection = tab.dataset.queryTab;
      for (const candidate of tabs) {
        const selected = candidate === tab;
        candidate.setAttribute("aria-selected", String(selected));
        candidate.tabIndex = selected ? 0 : -1;
      }
      root
        .querySelector("#query-panel")
        ?.setAttribute("aria-labelledby", tab.id);
      if (catalogListNode) catalogListNode.hidden = projection !== "catalogs";
      listNode.hidden = projection === "catalogs";
      if (actionsNode) actionsNode.hidden = projection === "catalogs";
      if (store) await render();
    });
    tab.addEventListener("keydown", (event) => {
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
      event.preventDefault();
      const target = tabs.find(
        (candidate) =>
          candidate.dataset.queryTab ===
          nextTab(tab.dataset.queryTab, event.key),
      );
      target?.click();
      target?.focus();
    });
  }

  try {
    const database = await openQueryDatabase();
    store = createQueryStore(createIndexedDbAdapter(database));
  } catch (error) {
    status(
      databaseFailureReason(error) === "unsupported-schema"
        ? t("library.unsupported_schema")
        : t("library.unavailable"),
    );
    return;
  }
  await captureQueue.attach(async (captured) => {
    const result = await store.recordSubmission(captured);
    if (!result.ok) {
      status(t("library.unavailable"));
      return;
    }
    await render();
  });

  function restore(entry) {
    restoreSource(textarea, entry.source);
    anchorSource = entry.source;
    renderEdited();
    // A restore must be visible: if the editor workspace is folded, unfold it
    // (app.js owns folding and listens for this).
    document.dispatchEvent(new CustomEvent("lingrep-web:query-restored"));
    focusEditor(textarea);
  }

  async function withEntry(id, action) {
    const result = await store.list();
    if (!result.ok) return;
    const entry = result.value.find((value) => value.id === id);
    if (entry) await action(entry);
  }

  panel.addEventListener(
    "pointerdown",
    () => {
      panelInteractionAt = Date.now();
    },
    { capture: true },
  );
  panel.addEventListener("click", async (event) => {
    const control = event.target.closest?.("[data-query-action]");
    if (!control) return;
    const action = control.dataset.queryAction;
    const id = control.dataset.queryId;
    const row = control.closest(".query-row");

    if (action === "restore") {
      await withEntry(id, restore);
      return;
    }
    if (action === "restore-run") {
      await withEntry(id, (entry) => {
        const run = () => {
          restore(entry);
          form.requestSubmit();
        };
        if (corpusMismatch(entry.corpusDigest, liveCorpusDigest(root))) {
          mismatchConfirmation(row, entry, run);
        } else {
          run();
        }
      });
      return;
    }
    if (action === "save" || action === "rename") {
      await withEntry(id, (entry) =>
        inlineNameForm(row, entry, async (name) => {
          const result =
            action === "save"
              ? await store.save({
                  source: entry.source,
                  name,
                  savedAt: new Date().toISOString(),
                  corpusDigest: entry.corpusDigest,
                })
              : await store.rename(id, name);
          if (!result.ok) {
            status(t("library.unavailable"));
            return;
          }
          anchorSource = entry.source;
          status(action === "save" ? t("library.saved_notice") : t("library.name_updated"));
          await render(true, { id, action: "rename" });
        }),
      );
      return;
    }

    const destructive = {
      "remove-recent": () => store.removeRecent(id),
      unsave: () => store.unsave(id),
      delete: () => store.delete(id),
    }[action];
    if (destructive) {
      const result = await destructive();
      if (!result.ok) {
        status(t("library.unavailable"));
        return;
      }
      undoRecords = result.value.removed;
      status(t("library.updated"), true);
      const nextAction =
        action === "unsave" ? "save" : action === "remove-recent" ? "rename" : null;
      await render(
        true,
        nextAction ? { id, action: nextAction } : { selector: "[data-query-status]" },
      );
    }
    if (action === "undo" && undoRecords?.length) {
      await store.restore(undoRecords);
      undoRecords = null;
      status(t("library.undone"));
      await render(true, { selector: "[data-query-status]" });
    }
  });

  panel.querySelector("[data-save-current]").addEventListener("click", () => {
    if (!textarea.value.trim()) {
      status(t("library.enter_pattern"));
      return;
    }
    const shell = document.createElement("div");
    shell.className = "query-row query-save-current";
    listNode.prepend(shell);
    const temporary = {
      source: textarea.value,
      firstDefinition: firstDefinition(textarea.value),
      name: null,
    };
    inlineNameForm(shell, temporary, async (name) => {
      const captured = textarea.value;
      const result = await store.save({
        source: captured,
        name,
        savedAt: new Date().toISOString(),
        corpusDigest: liveCorpusDigest(root),
      });
      if (!result.ok) {
        status(t("library.unavailable"));
        return;
      }
      anchorSource = captured;
      status(t("library.saved_notice"));
      await render(true, { selector: "[data-query-status]" });
    });
  });

  clearRecentButton.addEventListener("click", async () => {
    const result = await store.clearRecent();
    if (!result.ok) {
      status(t("library.unavailable"));
      return;
    }
    undoRecords = result.value.removed;
    status(t("library.cleared"), true);
    await render(true, { selector: "[data-clear-recent]" });
  });

  moreButton.addEventListener("click", async () => {
    savedLimit += SAVED_PAGE_SIZE;
    await render();
  });

  onLocaleChange(() => {
    if (store) void render();
  });

  textarea.addEventListener("input", renderEdited);
  root.addEventListener("htmx:afterSwap", (event) => {
    if (event.detail?.target?.matches?.(".corpus-context")) {
      void render();
    }
  });
  window.addEventListener("focus", () => {
    window.setTimeout(() => {
      if (Date.now() - panelInteractionAt < 500) return;
      void render();
    }, 100);
  });
  await render();
}

if (typeof document !== "undefined") {
  document.addEventListener("DOMContentLoaded", () => {
    void initializeQueryLibrary();
  });
}
