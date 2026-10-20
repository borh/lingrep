// Site-wide behavior for lingrep-web pages. Loaded as a module from layout.html;
// keeping it out of the template lets the CSP forbid inline scripts and lets
// the static-asset handler cache/revalidate it like every other asset.

import { t } from "./i18n.js";

/**
 * Whether a queued rerun should fire now.
 *
 * A cold corpus renders without a digest; the polled partial gains one the moment
 * the corpus is Ready, and a Failed corpus never does — so "has a digest" is the
 * readiness signal, and a failed build correctly never fires.
 */
export function shouldRunOnReady(form, contextEl) {
    if (!form || !contextEl) return false;
    if (!form.hasAttribute("data-run-when-ready")) return false;
    return Boolean(contextEl.getAttribute("data-corpus-digest"));
}

/**
 * What a corpus switch should do with `api/permalink`'s answer.
 *
 * Reported outcomes carry their i18n key and their English text together, the
 * way `LinkNotice` does server-side, so a locale that has not been fetched yet
 * still shows a sentence rather than a key. Anything that is not a minted
 * same-origin landing path reports: navigating to `undefined` is the one
 * outcome this exists to prevent.
 */
export function corpusSwitchOutcome(status, payload) {
    const failed = {
        action: "report",
        key: "corpus.switch_failed",
        text: "Couldn't switch corpus.",
    };
    if (status === 404) {
        return {
            action: "report",
            key: "corpus.switch_unknown",
            text: "That corpus is not available on this server.",
        };
    }
    if (status === 413) {
        return {
            action: "report",
            key: "corpus.switch_too_large",
            text: "Pattern too large to carry to another corpus.",
        };
    }
    if (status !== 200) return failed;
    // The same shape the permalink copier accepts: a query-only target and
    // nothing else, so a surprising body cannot redirect the browser somewhere
    // unrelated or out of the current mount.
    const path = payload?.path;
    if (typeof path !== "string" || !path.startsWith("?")) return failed;
    return { action: "navigate", path };
}

/**
 * The `api/permalink` request body.
 *
 * `corpus` is omitted when the page names none, which is what a single-corpus
 * server and every pre-existing caller send; the server then resolves its
 * default. Sending it is what keeps a copied link — and a corpus switch —
 * pointed at the corpus the page is actually on.
 */
export function permalinkRequestBody(pattern, corpus) {
    const params = new URLSearchParams({ pattern });
    if (corpus) params.set("corpus", corpus);
    return params.toString();
}

/**
 * The permalink request a corpus-search form should send — or the refusal to
 * send one at all.
 *
 * The form's hidden field names the corpus the page is pointed at, and this is
 * the only place that reads it, so a link cannot quietly lose its target. An
 * empty field means no slot resolved (an unknown-corpus landing): minting there
 * would hand back a link to the server's DEFAULT corpus, which is not the corpus
 * the user's own link named, so it is refused instead.
 */
export function permalinkRequestForForm(form) {
    const pattern = form?.querySelector("#corpus-pattern")?.value ?? "";
    const corpus = form?.querySelector("input[name=corpus]")?.value;
    if (!corpus) {
        return {
            action: "report",
            key: "corpus.select_before_link",
            text: "Choose a corpus before copying a link.",
        };
    }
    return { action: "request", body: permalinkRequestBody(pattern, corpus) };
}

/** The viewport at which the query band is pinned rather than in flow. */
export const BAND_PINNED_QUERY = "(min-width: 64rem)";

/**
 * The band's contribution to every other sticky offset on the page.
 *
 * Zero unless the band is actually pinned: below the breakpoint it is an
 * ordinary block that scrolls away, and the pattern rail and inspector dock
 * must keep the offsets they had before it existed. Rounded, because a
 * fractional offset leaves a hairline of the scrolled content showing above a
 * sticky header.
 */
export function stickyOffset(bandHeight, pinned) {
    if (!pinned || !Number.isFinite(bandHeight) || bandHeight <= 0) return 0;
    return Math.round(bandHeight);
}

export function shouldAutoRunInlineSample(drawerHidden, sampleText) {
    if (drawerHidden) return false;
    return typeof sampleText === "string" && sampleText.trim().length > 0;
}

// Everything below wires the live document. The guard is what lets a unit test
// import the pure helper above without a DOM — importing a module runs its top
// level — and mirrors the same guard in results-explorer.js.
if (typeof document !== "undefined") {
    // The corpus switch's live region. It lives OUTSIDE the corpus-context
    // section (see index.html), because that section is swapped wholesale by the
    // poll and would erase whatever was just reported into it.
    const announceCorpusStatus = ({ key }) => {
        const status = document.querySelector("[data-corpus-select-status]");
        if (!status) return;
        status.setAttribute("data-i18n", key);
        status.textContent = t(key);
    };

    // Copy-to-clipboard for inspection panels (capture lanes, token views).
    document.addEventListener("click", async (event) => {
        const button = event.target.closest("[data-copy-text]");
        if (!button) return;
        const region = button.closest(".inspection-target, .token-inspection");
        const report = (key) => {
            const status = region && region.querySelector(".copy-status");
            if (status) status.textContent = t(key);
        };
        if (!navigator.clipboard) {
            report("editor.copy_unavailable");
            return;
        }
        try {
            await navigator.clipboard.writeText(button.dataset.copyText);
            report("editor.copied");
        } catch (_error) {
            report("editor.copy_failed");
        }
    });

    // Shareable corpus-search links are encoded by the server so encoding and
    // decoding always use the same raw-DEFLATE implementation.
    document.addEventListener("click", async (event) => {
        const button = event.target.closest("[data-copy-permalink]");
        if (!button) return;
        const form = button.closest("[data-corpus-search]");
        const status = form?.querySelector(".permalink-status");
        const fallback = form?.querySelector(".permalink-fallback");
        const fallbackField = fallback?.querySelector(".permalink-url");
        const pattern = form?.querySelector("#corpus-pattern");
        if (!form || !pattern) return;

        const report = (key) => {
            if (!status) return;
            status.textContent = key ? t(key) : "";
        };
        if (fallback) fallback.hidden = true;
        const request = permalinkRequestForForm(form);
        if (request.action === "report") {
            // The remedy is at the selector, so the message goes to the
            // selector's region; clearing this button's own status keeps the two
            // live regions from disagreeing about what just happened.
            report("");
            announceCorpusStatus(request);
            return;
        }
        report("editor.creating_link");
        button.disabled = true;
        try {
            const response = await fetch("api/permalink", {
                method: "POST",
                headers: { "Content-Type": "application/x-www-form-urlencoded" },
                body: request.body,
            });
            if (response.status === 413) {
                report("editor.link_too_large");
                return;
            }
            // There is deliberately no "corpus isn't ready" branch: a cold corpus
            // has no identity to assert, so the server answers with a selector-only
            // link rather than the 503 it used to return.
            if (!response.ok) {
                report("editor.link_failed");
                return;
            }
            const payload = await response.json();
            if (typeof payload.path !== "string" || !payload.path.startsWith("?")) {
                report("editor.link_failed");
                return;
            }
            const url = new URL(payload.path, location.href).href;
            try {
                if (!navigator.clipboard) throw new Error("clipboard unavailable");
                await navigator.clipboard.writeText(url);
                report("editor.copied_link");
            } catch (_error) {
                if (fallback && fallbackField) {
                    fallbackField.value = url;
                    fallback.hidden = false;
                    fallbackField.focus();
                    fallbackField.select();
                }
                report("editor.link_fallback");
            }
        } catch (_error) {
            report("editor.link_failed");
        } finally {
            button.disabled = false;
        }
    });

    document.addEventListener("click", (event) => {
        const button = event.target.closest("[data-dismiss-permalink]");
        button?.closest(".permalink-banner")?.remove();
    });

    // Switching corpora NAVIGATES rather than re-pointing this page: a workbench
    // is bound to one corpus when it is admitted and cannot be rebound, so a
    // different corpus needs a new workbench — which a fresh landing gives for
    // free. The server mints that landing from the current editor contents and
    // the newly selected key, emitting `k` alone for a corpus that has never
    // been built, so a cold switch lands in `preparing` rather than in a
    // spurious identity-mismatch banner.
    document.addEventListener("change", async (event) => {
        const select = event.target.closest?.("[data-corpus-select]");
        if (!select) return;
        const pattern = document.getElementById("corpus-pattern");

        const report = (outcome) => {
            // The served option carries `selected`, so restoring it leaves the
            // control naming the corpus the page is actually still pointed at.
            const served = select.querySelector("option[selected]");
            if (served) select.value = served.value;
            announceCorpusStatus(outcome);
        };

        select.disabled = true;
        try {
            const response = await fetch("api/permalink", {
                method: "POST",
                headers: { "Content-Type": "application/x-www-form-urlencoded" },
                body: permalinkRequestBody(pattern?.value ?? "", select.value),
            });
            const payload = await response.json().catch(() => null);
            const outcome = corpusSwitchOutcome(response.status, payload);
            if (outcome.action === "navigate") {
                location.assign(outcome.path);
                return;
            }
            report(outcome);
        } catch (_error) {
            report(corpusSwitchOutcome(0, null));
        } finally {
            select.disabled = false;
        }
    });

    // Only the server emits data-autorun, after verifying the link's corpus handle.
    window.addEventListener("DOMContentLoaded", () => {
        document
            .querySelector("[data-corpus-search][data-autorun]")
            ?.requestSubmit();
    });

    // Fire the queued rerun after each corpus-context poll swaps in a new partial.
    // The attribute is removed before submitting, so this is one-shot even though
    // the poll keeps running.
    document.body.addEventListener("htmx:afterSwap", (event) => {
        if (!event.target.matches?.(".corpus-context")) return;
        const form = document.querySelector("[data-corpus-search]");
        if (!shouldRunOnReady(form, event.target)) return;
        form.removeAttribute("data-run-when-ready");
        form.requestSubmit();
    });

    // A landing notice explains why the seeded pattern did NOT run. Once
    // something runs it is describing a page that no longer exists, and
    // `notice.corpus_loading` in particular sat there after the corpus had
    // finished loading and the queued query had already produced results.
    // Removed on the request, not on its response: the notice is stale from
    // the moment a run is under way.
    document.addEventListener("htmx:beforeRequest", (event) => {
        if (!event.detail.elt.closest?.("form[data-corpus-search]")) return;
        for (const notice of document.querySelectorAll(".permalink-notice")) {
            notice.remove();
        }
    });

    // Publish the pinned band's height so every other sticky region on the
    // page clears it instead of sliding underneath. The band's height is not a
    // constant: the editor resizes, the syntax help and the query library open
    // inside it, and the run status grows a progress line.
    (function () {
        const band = document.querySelector("[data-query-band]");
        if (!band) return;
        const pinned = matchMedia(BAND_PINNED_QUERY);
        const apply = () => {
            const offset = stickyOffset(
                band.getBoundingClientRect().height,
                pinned.matches,
            );
            document.documentElement.style.setProperty(
                "--sticky-top",
                `${offset}px`,
            );
        };
        new ResizeObserver(apply).observe(band);
        pinned.addEventListener("change", apply);
        apply();
    })();

    // Syntax help opens over the page rather than inside the band. As an
    // inline `details` it was a flex item in the controls row, so opening it
    // rendered its paragraph in a narrow detached column and grew the band by
    // 64px — reference text displacing the editor it describes. `dialog`
    // supplies Escape, the backdrop and focus containment; there is nothing to
    // implement here but open and close.
    (function () {
        const dialog = () => document.querySelector("[data-editor-help]");
        document.addEventListener("click", (event) => {
            if (event.target.closest("[data-editor-help-open]")) {
                dialog()?.showModal();
                return;
            }
            if (event.target.closest("[data-editor-help-close]")) dialog()?.close();
            for (const openScope of document.querySelectorAll("details.corpus-analysis-scope[open]")) {
                if (!openScope.contains(event.target)) {
                    openScope.open = false;
                }
            }
        });
        document.addEventListener("keydown", (event) => {
            if (event.key !== "Escape") return;
            for (const openScope of document.querySelectorAll("details.corpus-analysis-scope[open]")) {
                openScope.open = false;
            }
        });
    })();

    (function () {
        const drawer = () => document.querySelector("[data-inline-sample-drawer]");
        const toggle = () => document.querySelector("[data-sample-drawer-toggle]");
        const sampleInput = () => document.querySelector("[data-inline-sample-text]");
        const resultsEl = () => document.querySelector("[data-inline-sample-results]");
        const statusEl = () => document.querySelector("[data-inline-sample-status]");
        const patternInput = () => document.getElementById("corpus-pattern");

        const setOpen = (open) => {
            const d = drawer();
            const btn = toggle();
            if (!d || !btn) return;
            d.hidden = !open;
            btn.setAttribute("aria-expanded", String(open));
            if (open) {
                sampleInput()?.focus();
                if (!resultsEl()?.hasChildNodes() && sampleInput()?.value.trim()) {
                    void runInlineSample();
                }
            }
        };

        let sampleAbort = null;
        let sampleInputTimer = null;

        const runInlineSample = async () => {
            const pattern = patternInput()?.value ?? "";
            const sample = sampleInput()?.value ?? "";
            const status = statusEl();
            const target = resultsEl();
            if (!target) return;
            sampleAbort?.abort();
            const controller = new AbortController();
            sampleAbort = controller;
            if (status) status.textContent = t("sample.running");
            target.setAttribute("aria-busy", "true");
            try {
                const body = new URLSearchParams({ pattern, sample }).toString();
                const response = await fetch("api/sample", {
                    method: "POST",
                    headers: { "Content-Type": "application/x-www-form-urlencoded" },
                    body,
                    signal: controller.signal,
                });
                if (sampleAbort !== controller) return;
                const html = await response.text();
                if (sampleAbort !== controller) return;
                const parsed = new DOMParser().parseFromString(html, "text/html");
                target.replaceChildren(...parsed.body.childNodes);
                if (status) status.textContent = "";
            } catch (error) {
                if (error?.name === "AbortError") return;
                if (status) status.textContent = t("sample.run_failed");
            } finally {
                if (sampleAbort === controller) {
                    target.setAttribute("aria-busy", "false");
                }
            }
        };

        document.addEventListener("pattern:valid", (event) => {
            if (event.target?.id !== "corpus-pattern") return;
            if (shouldAutoRunInlineSample(Boolean(drawer()?.hidden), sampleInput()?.value)) {
                void runInlineSample();
            }
        });

        document.addEventListener("input", (event) => {
            if (!event.target.matches?.("[data-inline-sample-text]")) return;
            clearTimeout(sampleInputTimer);
            sampleInputTimer = setTimeout(() => {
                if (shouldAutoRunInlineSample(Boolean(drawer()?.hidden), sampleInput()?.value)) {
                    void runInlineSample();
                }
            }, 300);
        });

        document.addEventListener("click", (event) => {
            if (event.target.closest("[data-sample-drawer-toggle]")) {
                setOpen(Boolean(drawer()?.hidden));
                return;
            }
            if (event.target.closest("[data-sample-drawer-close]")) {
                setOpen(false);
                return;
            }
            if (event.target.closest("[data-run-inline-sample]")) {
                void runInlineSample();
            }
        });
    })();

    // Editor fold: the query workspace collapses to a one-line bar once a search
    // is accepted, keeping the edit→run→inspect loop inside one viewport.
    // Ctrl+Enter runs the query from anywhere; Ctrl+E folds/expands the editor.
    (function () {
        const COLLAPSED_KEY = "lingrep-web:editor-collapsed:v1";
        const workspace = () => document.querySelector("[data-query-workspace]");
        const bar = () => workspace()?.querySelector("[data-editor-collapsed-bar]");
        const form = () => workspace()?.querySelector("form[data-corpus-search]");
        const pattern = () => document.getElementById("corpus-pattern");

        // Preview the folded editor by intent, not by first line: the first
        // non-comment line of a catalog is typically a grammar-level statement
        // (`defaults`, `set X := {…}`, `fragment Y := …`) and means nothing to a
        // researcher. `patternDefinitions` is the editor's own definition index, so
        // preview semantics cannot drift ahead of navigation semantics.
        let patternEditorModule = null;
        const loadPatternEditor = () =>
            (patternEditorModule ??= import("./editor/pattern-editor.js").catch(
                () => null,
            ));
        const previewText = async () => {
            try {
                const module = await loadPatternEditor();
                const label = t("editor.folded.label");
                if (!module) return label;
                return module.patternPreview(pattern()?.value ?? "", label);
            } catch (_error) {
                return t("editor.folded.label");
            }
        };

        const setCollapsed = (collapsed, { persist = true } = {}) => {
            const ws = workspace();
            const toggle = bar();
            if (!ws || !toggle) return;
            ws.dataset.editorCollapsed = String(collapsed);
            toggle.hidden = !collapsed;
            toggle.setAttribute("aria-expanded", String(!collapsed));
            if (collapsed) {
                const preview = toggle.querySelector("[data-editor-preview]");
                void previewText().then((text) => {
                    preview.textContent = text;
                });
            }
            if (persist) {
                try {
                    localStorage.setItem(COLLAPSED_KEY, String(collapsed));
                } catch (_error) {
                    // Preference persistence is non-blocking.
                }
            }
        };

        const storedCollapsed = () => {
            try {
                return localStorage.getItem(COLLAPSED_KEY) === "true";
            } catch (_error) {
                return false;
            }
        };

        document.addEventListener("DOMContentLoaded", () => {
            if (workspace()) setCollapsed(storedCollapsed(), { persist: false });
            bar()?.addEventListener("click", () => setCollapsed(false));
        });
        document.addEventListener("click", (event) => {
            if (event.target.closest("[data-editor-fold]")) setCollapsed(true);
        });
        // The DOMContentLoaded listener above is registered from a `defer` script
        // and runs before the event fires, so no missed-event guard is needed.

        // Fold the editor once a corpus search has been accepted — but only
        // where folding buys something. A pinned band no longer displaces the
        // results, so folding it after every run would just hide the pattern
        // the next edit is about.
        document.addEventListener("htmx:afterRequest", (event) => {
            if (matchMedia(BAND_PINNED_QUERY).matches) return;
            const submitted = event.detail.elt.closest?.("form[data-corpus-search]");
            if (submitted && event.detail.successful) setCollapsed(true);
        });

        // Loading a query from the library must reveal it.
        document.addEventListener("lingrep-web:query-restored", () => setCollapsed(false));

        document.addEventListener(
            "keydown",
            (event) => {
                if (!(event.ctrlKey || event.metaKey) || event.altKey || event.shiftKey) {
                    return;
                }
                if (event.key === "Enter") {
                    if (event.target.closest?.("[data-inline-sample-drawer]")) {
                        event.preventDefault();
                        document.querySelector("[data-run-inline-sample]")?.click();
                        return;
                    }
                    const target = form() ?? document.querySelector(".sample-form");
                    if (!target) return;
                    event.preventDefault();
                    target.requestSubmit();
                    return;
                }
                if (event.key === "e" || event.key === "E") {
                    const ws = workspace();
                    if (!ws) return;
                    event.preventDefault();
                    setCollapsed(ws.dataset.editorCollapsed !== "true");
                }
            },
            { capture: true },
        );
    })();

    // Streamed corpus searches: tell the user when the connection dies and offer
    // a one-click retry. EventSource auto-retries transient errors, so the notice
    // is debounced and withdrawn if the stream recovers; a stream that already
    // delivered its terminal #results swap never gets the notice. The notice
    // lives inside #search-status, so starting a new search replaces it.
    (function () {
        const SHOW_DELAY_MS = 1500;

        const streamElement = (target) =>
            target && target.id === "search-stream" ? target : target?.closest?.("#search-stream");

        const clearNotice = (stream) => {
            if (!stream) return;
            clearTimeout(stream.streamLostTimer);
            stream.streamLostTimer = undefined;
            stream.parentElement?.querySelector(".stream-lost")?.remove();
        };

        const showNotice = (stream) => {
            if (stream.parentElement?.querySelector(".stream-lost")) return;
            const notice = document.createElement("div");
            notice.className = "stream-lost";
            notice.setAttribute("role", "alert");
            const message = document.createElement("p");
            message.dataset.i18n = "status.stream_lost";
            message.textContent = t("status.stream_lost");
            const retry = document.createElement("button");
            retry.type = "button";
            retry.dataset.i18n = "status.stream_retry";
            retry.textContent = t("status.stream_retry");
            retry.addEventListener("click", () => {
                document.querySelector("[data-corpus-search]")?.requestSubmit();
            });
            notice.append(message, retry);
            stream.after(notice);
        };

        // The terminal "result" SSE event swaps #results out of band (stream.rs).
        document.body.addEventListener("htmx:oobAfterSwap", (event) => {
            if (event.detail?.target?.id === "results") {
                const stream = document.getElementById("search-stream");
                if (stream) stream.dataset.streamFinished = "true";
            }
        });

        document.body.addEventListener("htmx:sseError", (event) => {
            const stream = streamElement(event.target);
            if (!stream || stream.dataset.streamFinished === "true") return;
            clearTimeout(stream.streamLostTimer);
            stream.streamLostTimer = setTimeout(() => showNotice(stream), SHOW_DELAY_MS);
        });

        // Reconnects fire htmx:sseOpen; any recovery withdraws the pending notice.
        document.body.addEventListener("htmx:sseOpen", (event) => {
            clearNotice(streamElement(event.target));
        });
    })();
}
