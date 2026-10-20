import test from "node:test";
import assert from "node:assert/strict";

import {
  DETAIL_VIEWS,
  activationKey,
  applyFilterEmptyNote,
  availableDetailViews,
  resolveDetailView,
  compareCatalogRows,
  rateLabel,
  rateTenths,
  emptyHitState,
  filterStatus,
  groupLabelNode,
  isOverflowing,
  namedMaximum,
  nextRowIndex,
  pageProjection,
  pageRangeLabel,
  parseKwicPageSize,
  rankedEntryVisible,
  rankedTail,
  rankedTailSummary,
  searchMatches,
  transitionHitState,
  transitionPageState,
  visibleForQuery,
  visibleForSection,
  visibleForVariation,
} from "../assets/results-explorer.js";

function fakeDocument() {
  return {
    createElement() {
      return {
        children: [],
        className: "",
        textContent: "",
        append(...children) {
          this.children.push(...children);
        },
      };
    },
  };
}

test("group headings keep units out of the numeric value", () => {
  const heading = groupLabelNode(fakeDocument(), "Modality", "72,730");
  assert.equal(heading.children[0].textContent, "Modality");
  assert.equal(heading.children[1].className, "pattern-group-count");
  assert.equal(heading.children[1].textContent, "72,730");
});

test("KWIC page-size preference accepts only the product vocabulary", () => {
  for (const value of ["50", "100", "250", "500", "1000"]) {
    assert.equal(parseKwicPageSize(value), Number(value));
  }
  for (const value of [null, "", "0", "49", "1001", "garbage"]) {
    assert.equal(parseKwicPageSize(value), 100);
  }
});

test("page projection clamps and preserves document order", () => {
  const rows = Array.from({ length: 251 }, (_, index) => index);
  assert.deepEqual(pageProjection(rows, 2, 100), {
    pageIndex: 2,
    pageCount: 3,
    start: 200,
    end: 251,
    rows: rows.slice(200),
  });
  assert.equal(pageProjection(rows, 99, 100).pageIndex, 2);
  assert.deepEqual(pageProjection([], 4, 100), {
    pageIndex: 0,
    pageCount: 1,
    start: 0,
    end: 0,
    rows: [],
  });
});

test("page transitions reset for filters and page size", () => {
  const state = { pageIndex: 4, pageSize: 100 };
  assert.deepEqual(transitionPageState(state, { type: "filter" }), {
    pageIndex: 0,
    pageSize: 100,
  });
  assert.deepEqual(transitionPageState(state, {
    type: "page-size",
    value: 500,
  }), {
    pageIndex: 0,
    pageSize: 500,
  });
  assert.deepEqual(transitionPageState(state, { type: "next", pageCount: 8 }), {
    pageIndex: 5,
    pageSize: 100,
  });
});

test("page range reports the visible slice and active scopes", () => {
  assert.equal(
    pageRangeLabel(100, 143, 143, "食べる", "methods"),
    "101–143 of 143 retained hits · Surface “食べる” · Section “methods”",
  );
  assert.equal(
    pageRangeLabel(0, 12, 12, "食べる", "methods", "重要"),
    "1–12 of 12 retained hits · Surface “食べる” · Section “methods” · Matching “重要”",
  );
  assert.equal(
    pageRangeLabel(0, 0, 0, null, undefined),
    "0 retained hits · All surfaces",
  );
});

test("concordance text filtering matches substrings case-insensitively and ignores whitespace", () => {
  assert.equal(visibleForQuery("この結果は重要であると考えられる。", ""), true);
  assert.equal(visibleForQuery("この結果は重要であると考えられる。", "   "), true);
  assert.equal(visibleForQuery("この結果は重要であると考えられる。", "重要"), true);
  assert.equal(visibleForQuery("この結果は重要であると考えられる。", "  重要 "), true);
  assert.equal(visibleForQuery("この結果は重要であると考えられる。", "不在"), false);
});

test("variation filtering uses structural identity", () => {
  assert.equal(visibleForVariation("7", "7"), true);
  assert.equal(visibleForVariation("7", "8"), false);
  assert.equal(visibleForVariation("7", "all"), true);
});

test("an emptied surface filter states why instead of reading as broken", () => {
  const note = { hidden: true };
  const detail = {
    querySelector: (selector) =>
      selector === "[data-kwic-filter-empty]" ? note : null,
  };
  // A surface with a positive corpus count and no retained row: the note shows.
  applyFilterEmptyNote(detail, 0, "7", "all");
  assert.equal(note.hidden, false);
  // No selection, a section-only selection, or a surviving row: it does not.
  applyFilterEmptyNote(detail, 0, "all", "all");
  assert.equal(note.hidden, true);
  applyFilterEmptyNote(detail, 0, "all", "2");
  assert.equal(note.hidden, true);
  applyFilterEmptyNote(detail, 0, "7", "2");
  assert.equal(note.hidden, true);
  applyFilterEmptyNote(detail, 3, "7", "all");
  assert.equal(note.hidden, true);
  applyFilterEmptyNote(detail, 0, "7", "all", "重要");
  assert.equal(note.hidden, true);
  // A pane without the note is a no-op, not a crash.
  assert.equal(
    applyFilterEmptyNote({ querySelector: () => null }, 0, "7", "all"),
    undefined,
  );
});

test("ranked entries use one collapsed or expanded value set", () => {
  assert.equal(rankedEntryVisible(0, 19, false, true), true);
  assert.equal(rankedEntryVisible(18, 19, false, true), true);
  assert.equal(rankedEntryVisible(19, 19, false, true), false);
  assert.equal(rankedEntryVisible(19, 19, true, true), true);
  assert.equal(rankedEntryVisible(24, 19, false, false), false);
});

test("search reveals matching entries beyond the overview", () => {
  assert.equal(rankedEntryVisible(24, 19, false, true, true), true);
});

test("tail counts exactly the hidden individual entries", () => {
  assert.deepEqual(
    rankedTail([{ count: 8 }, { count: 5 }, { count: 3 }], 1),
    { entryCount: 2, hitCount: 8 },
  );
});

test("named chart maximum excludes the aggregate tail", () => {
  assert.equal(namedMaximum([{ count: 35 }, { count: 34 }, { count: 29 }]), 35);
  assert.equal(
    namedMaximum([{ count: 35 }, { count: 34 }, { count: 80 }]),
    80,
    "a real no-section category shares the named-entry denominator",
  );
  assert.equal(namedMaximum([]), 1);
});

test("ranked tail is a textual aggregate", () => {
  // Surfaces only. The section list is never truncated, so it has no tail to
  // summarize.
  assert.equal(
    rankedTailSummary(981, 656, 1000),
    "Other: 656 hits across 981 variations (65.6% of retained hits)",
  );
  assert.equal(rankedTailSummary(0, 0, 0), "Other: 0 hits across 0 variations (0.0% of retained hits)");
});

test("status labels a retained-sample subset", () => {
  assert.equal(
    filterStatus(3, 10, "食べる"),
    "3 of 10 retained hits · Surface “食べる”",
  );
});

test("section filtering matches membership, all, and the empty bucket", () => {
  assert.equal(visibleForSection(["0", "2"], "all"), true);
  assert.equal(visibleForSection(["0", "2"], "2"), true);
  assert.equal(visibleForSection(["0", "2"], "3"), false);
  assert.equal(visibleForSection([], "none"), true);
  assert.equal(visibleForSection(["0"], "none"), false);
});

test("status names both scopes, stating an explicit 'all' rather than dropping it", () => {
  assert.equal(
    filterStatus(40, 40, null, null),
    "40 retained hits · All surfaces · All sections",
  );
  assert.equal(
    filterStatus(12, 40, "食べる", "methods"),
    "12 of 40 retained hits · Surface “食べる” · Section “methods”",
  );
});

test("status omits the section scope when there is no section dimension", () => {
  assert.equal(
    filterStatus(40, 40, null),
    "40 retained hits · All surfaces",
  );
});

test("grid movement skips hidden rows and wraps", () => {
  assert.equal(nextRowIndex(0, 1, [true, false, true]), 2);
  assert.equal(nextRowIndex(2, 1, [true, false, true]), 0);
});

test("catalog search is case-normalized substring matching", () => {
  assert.equal(searchMatches("ABC表層", "bc表"), true);
});

test("focused rows activate only with Enter or Space", () => {
  assert.equal(activationKey("Enter"), true);
  assert.equal(activationKey(" "), true);
  assert.equal(activationKey("ArrowDown"), false);
});

test("catalog comparator supports frequency and surface ordering", () => {
  const a = { form: "b", count: 1 };
  const b = { form: "a", count: 2 };
  assert.ok(compareCatalogRows(a, b, "count", "desc") > 0);
  assert.ok(compareCatalogRows(a, b, "form", "asc") > 0);
});

test("the section comparator ranks on the rate and breaks ties by label", () => {
  // More hits, lower rate: the count order and the rate order disagree, which
  // is the whole reason the section panel ranks on the rate.
  const busy = { form: "results", count: 80, rate: 200 };
  const dense = { form: "methods", count: 20, rate: 2000 };
  assert.ok(compareCatalogRows(dense, busy, "rate", "desc") < 0);
  assert.ok(compareCatalogRows(dense, busy, "count", "desc") > 0);
  // Equal rates fall to the label, so a re-render under a surface filter
  // cannot shuffle rows that tie.
  const first = { form: "a", count: 1, rate: 500 };
  const second = { form: "b", count: 9, rate: 500 };
  assert.ok(compareCatalogRows(first, second, "rate", "desc") < 0);
});

test("the client's rate matches the server's, rounding included", () => {
  // `pattern_detail.rs`'s `rate_tenths`: tenths of a ppm, rounded half up.
  assert.equal(rateTenths(1, 1_000_000), 10);
  assert.equal(rateTenths(4, 4), 10_000_000);
  assert.equal(rateTenths(0, 40), 0);
  assert.equal(rateTenths(3, 0), 0, "an empty section has no rate, not NaN");
  // 1/3 of a million is 333333.33 ppm, so 3333333 tenths.
  assert.equal(rateTenths(1, 3), 3_333_333);
  assert.equal(rateLabel(3_333_333), "333333.3");
  assert.equal(rateLabel(10), "1.0");
  assert.equal(rateLabel(0), "0.0");
});

test("selecting never changes sentence expansion", () => {
  const current = {
    selectedOrdinal: "4",
    expandedOrdinal: "9",
    inspectorOrdinal: "4",
  };
  assert.deepEqual(transitionHitState(current, {
    type: "select",
    ordinal: "7",
  }), {
    state: {
      selectedOrdinal: "7",
      expandedOrdinal: "9",
      inspectorOrdinal: "4",
    },
    requestOrdinal: "7",
  });
});

test("re-selecting a previously inspected row while another is pending re-requests it", () => {
  const current = {
    selectedOrdinal: "7",
    expandedOrdinal: null,
    inspectorOrdinal: "4",
  };
  assert.equal(
    transitionHitState(current, { type: "select", ordinal: "4" }).requestOrdinal,
    "4",
  );
});

test("sentence expansion never changes selection or inspection", () => {
  const current = {
    selectedOrdinal: "4",
    expandedOrdinal: null,
    inspectorOrdinal: "4",
  };
  assert.deepEqual(
    transitionHitState(current, { type: "toggle-expansion", ordinal: "9" }).state,
    {
      selectedOrdinal: "4",
      expandedOrdinal: "9",
      inspectorOrdinal: "4",
    },
  );
});

test("expanding another sentence replaces the single expanded ordinal", () => {
  const current = {
    selectedOrdinal: "4",
    expandedOrdinal: "9",
    inspectorOrdinal: "4",
  };
  assert.deepEqual(
    transitionHitState(current, {
      type: "toggle-expansion",
      ordinal: "7",
    }).state,
    {
      selectedOrdinal: "4",
      expandedOrdinal: "7",
      inspectorOrdinal: "4",
    },
  );
});

test("closing inspection preserves sentence expansion", () => {
  const current = { selectedOrdinal: "4", expandedOrdinal: "9", inspectorOrdinal: "4" };
  assert.deepEqual(
    transitionHitState(current, { type: "close-inspector" }).state,
    { selectedOrdinal: null, expandedOrdinal: "9", inspectorOrdinal: null },
  );
});

test("escape on the expanded sentence control collapses only that sentence", () => {
  const current = { selectedOrdinal: "4", expandedOrdinal: "9", inspectorOrdinal: "4" };
  assert.deepEqual(
    transitionHitState(current, {
      type: "escape", ordinal: "9", focusedOnExpansion: true,
    }).state,
    { selectedOrdinal: "4", expandedOrdinal: null, inspectorOrdinal: "4" },
  );
});

test("escape elsewhere closes inspection and preserves expansion", () => {
  const current = { selectedOrdinal: "4", expandedOrdinal: "9", inspectorOrdinal: "4" };
  assert.deepEqual(
    transitionHitState(current, {
      type: "escape", ordinal: "4", focusedOnExpansion: false,
    }).state,
    { selectedOrdinal: null, expandedOrdinal: "9", inspectorOrdinal: null },
  );
});

test("overflow measurement tolerates one css pixel of rounding", () => {
  assert.equal(isOverflowing(201, 200), false);
  assert.equal(isOverflowing(202, 200), true);
});

test("a tab with no panel is not a view the selection may fall on", () => {
  // A run may produce no dendrogram, no sections at all, or a pattern whose
  // definition the catalog no longer holds.
  assert.deepEqual(
    availableDetailViews(["definition", "concordance"]),
    ["concordance", "definition"],
    "availability is reported in tab order, not in discovery order",
  );
  assert.deepEqual(availableDetailViews([]), []);
  assert.deepEqual(availableDetailViews(DETAIL_VIEWS), DETAIL_VIEWS);
});

test("a selection whose panel went away falls back, and no choice prefers the default", () => {
  assert.equal(resolveDetailView("sections", ["concordance", "sections"]), "sections");
  // Switching to a pattern with no section counts must not leave the panel
  // showing nothing at all.
  assert.equal(resolveDetailView("sections", ["concordance"]), "concordance");
  assert.equal(resolveDetailView("concordance", []), null);

  // Before the first detail arrives the only panels on the page are the
  // catalog-level ones, so the concordance is briefly unavailable. Having no
  // choice must not be recorded as choosing what was there at that moment.
  assert.equal(resolveDetailView(null, ["sections"]), "sections");
  assert.equal(resolveDetailView(null, ["sections", "concordance"]), "concordance");
  assert.equal(resolveDetailView(null, []), null);
});
