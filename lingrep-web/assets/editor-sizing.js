import { localize } from "./i18n.js";

export const EDITOR_HEIGHT_KEY = "lingrep-web:editor-height:v1";
export const EDITOR_EXPANDED_KEY = "lingrep-web:editor-expanded:v1";

export function parseEditorHeight(value, viewportHeight, rootFontSize) {
  if (value === null || value === "") return null;
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return null;
  // 4.5rem, not 12: inside the pinned query band the editor is four lines by
  // design, and a 12rem floor would silently restore a stored height the band
  // has no room for.
  return Math.min(Math.max(parsed, 4.5 * rootFontSize), 0.9 * viewportHeight);
}

export function editorExpanded(value) {
  return value === "true";
}

function read(storage, key) {
  try {
    return storage.getItem(key);
  } catch (_error) {
    return null;
  }
}

function write(storage, key, value) {
  try {
    storage.setItem(key, value);
  } catch (_error) {
    // Sizing is a non-blocking preference.
  }
}

/**
 * Retarget the toggle's translated label rather than replacing the button's
 * text.
 *
 * Writing `toggle.textContent` deleted the `data-i18n` span the template
 * renders, so the label stopped being translated the first time the button
 * was pressed and stayed English for the rest of the session. Rewriting the
 * span's key instead leaves i18n's `localize` pass something to act on.
 */
export function setToggleLabel(toggle, expanded) {
  const label = toggle.querySelector("[data-i18n]") ?? toggle;
  label.setAttribute("data-i18n", expanded ? "editor.collapse" : "editor.expand");
  label.textContent = expanded ? "Collapse editor" : "Expand editor";
  return label;
}

/** Re-runs the translation pass over one relabelled element. */
function relocalize(element) {
  if (typeof document === "undefined") return;
  localize(element);
}

export function initializeEditorSizing(
  root = document,
  storage = localStorage,
) {
  const wrapper = root.querySelector("[data-editor-size]");
  const toggle = root.querySelector("[data-editor-toggle]");
  if (!wrapper || !toggle || wrapper.dataset.editorSizing === "ready") return;
  wrapper.dataset.editorSizing = "ready";

  const rootFontSize = Number.parseFloat(
    getComputedStyle(root.documentElement).fontSize,
  );
  const storedHeight = parseEditorHeight(
    read(storage, EDITOR_HEIGHT_KEY),
    innerHeight,
    rootFontSize,
  );
  if (storedHeight !== null) {
    wrapper.style.setProperty("--corpus-editor-height", `${storedHeight}px`);
  }

  const applyExpanded = (expanded) => {
    wrapper.dataset.editorExpanded = String(expanded);
    toggle.setAttribute("aria-expanded", String(expanded));
    relocalize(setToggleLabel(toggle, expanded));
    write(storage, EDITOR_EXPANDED_KEY, String(expanded));
  };
  applyExpanded(editorExpanded(read(storage, EDITOR_EXPANDED_KEY)));

  toggle.addEventListener("click", () => {
    applyExpanded(toggle.getAttribute("aria-expanded") !== "true");
  });

  let pointerResize = false;
  wrapper.addEventListener("pointerdown", () => {
    pointerResize = true;
  });
  window.addEventListener("pointerup", () => {
    if (!pointerResize) return;
    pointerResize = false;
    const height = parseEditorHeight(
      wrapper.getBoundingClientRect().height,
      innerHeight,
      rootFontSize,
    );
    if (height !== null && toggle.getAttribute("aria-expanded") !== "true") {
      wrapper.style.setProperty("--corpus-editor-height", `${height}px`);
      write(storage, EDITOR_HEIGHT_KEY, String(height));
    }
  });
}

if (typeof document !== "undefined") {
  window.addEventListener("DOMContentLoaded", () => initializeEditorSizing());
}
