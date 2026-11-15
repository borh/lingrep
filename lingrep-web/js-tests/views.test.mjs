import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { currentLocale, initI18n } from "../assets/i18n.js";
import { conditionText, fieldName, phrase, reasonText, scopeDocuments } from "../assets/views.js";

async function useJapanese() {
  globalThis.localStorage = { getItem: () => "ja", setItem() {} };
  globalThis.fetch = async (url) => ({ ok: true, json: async () => JSON.parse(readFileSync(fileURLToPath(url), "utf8")) });
  globalThis.document = { documentElement: { dataset: {} }, addEventListener() {}, querySelector: () => null, querySelectorAll: () => [] };
  globalThis.MutationObserver = class { observe() {} };
  await initI18n();
  assert.equal(currentLocale(), "ja");
}

const genre = { kind: "categorical", key: "genre" };
const year = { kind: "range", key: "year" };
const control = (name, any = false) => ({ name, any, html: `<button>${name}</button>` });

function texts() {
  return {
    name: fieldName("publication_year"),
    any: conditionText(genre, { m: "in", v: [] }, "genre"),
    one: conditionText(genre, { m: "in", v: ["novel"] }, "genre"),
    three: conditionText(genre, { m: "in", v: ["novel", "essay", "poem"] }, "genre"),
    four: conditionText(genre, { m: "in", v: ["a", "b", "c", "d"] }, "genre"),
    except: conditionText(genre, { m: "out", v: ["novel", "essay"] }, "genre"),
    separated: conditionText(genre, { m: "in", v: ["詩・短歌", "小説"] }, "genre"),
    numbers: conditionText(genre, { m: "in", v: [1, 2] }, "genre"),
    between: conditionText(year, { m: "range", lo: 1900, hi: 1950 }, "year"),
    from: conditionText(year, { m: "range", lo: 1900 }, "year"),
    to: conditionText(year, { m: "range", hi: 1950 }, "year"),
    unbounded: conditionText(year, { m: "range" }, "year"),
    documents: scopeDocuments([control("genre", true), control("ndc", true), control("year")]),
    anyLast: scopeDocuments([control("year"), control("genre", true)]),
    none: scopeDocuments([]),
    reason: reasonText({ reason: "unknown_key", key: "genre" }),
  };
}

test("condition and scope text in each locale", async () => {
  assert.deepEqual(texts(), {
    name: "publication year",
    any: "any genre",
    one: "novel",
    three: "novel, essay or poem",
    four: "4 values",
    except: "other than novel and essay",
    separated: "詩・短歌 or 小説",
    numbers: "1 or 2",
    between: "1900–1950",
    from: "1900 or more",
    to: "1950 or less",
    unbounded: "any year",
    documents: "documents with <button>genre</button>, with <button>ndc</button>, with year <button>year</button>",
    anyLast: "documents with year <button>year</button>, with <button>genre</button>",
    none: "",
    reason: "This corpus has no condition named “genre”.",
  });
  await useJapanese();
  assert.deepEqual(texts(), {
    name: "publication_year",
    any: "genre",
    one: "novel",
    three: "novel・essay・poem",
    four: "4件",
    except: "novel・essay以外",
    separated: "詩・短歌、小説",
    numbers: "1・2",
    between: "1900–1950",
    from: "1900以上",
    to: "1950以下",
    unbounded: "year",
    documents: "<button>genre</button>・<button>ndc</button>を問わない、yearが<button>year</button>の文書",
    anyLast: "yearが<button>year</button>、<button>genre</button>を問わない文書",
    none: "",
    reason: "このコーパスに「genre」という条件はありません。",
  });
});

test("a reason without a string is shown as received", () => {
  assert.equal(reasonText({ reason: "not_a_known_reason" }), "not_a_known_reason");
});

test("a slot's content is not read as another slot", () => {
  const html = phrase("views.scope_is", { name: "{value}", value: "V" });
  assert.equal(html.split("{value}").length, 2);
  assert.equal(html.split("V").length, 2);
});
