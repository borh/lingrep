import test from "node:test";
import assert from "node:assert/strict";
import { labelForms } from "../assets/frequency.js";

const OPEN = "（([［「", CLOSE = "）)]］」";
const SEPARATORS = "、・ 　";

function random(seed) {
  let x = seed >>> 0 || 1;
  return (n) => {
    x ^= x << 13;
    x ^= x >>> 17;
    x ^= x << 5;
    return (x >>> 0) % n;
  };
}

const WORDS = ["日本", "文学", "小説", "物語", "詩歌", "評論", "随筆", "Tokyo", "Kita", "東京府", "第一", "全集", "第2巻"];
function name(pick, depth = 0) {
  let out = "";
  const parts = 1 + pick(4);
  for (let i = 0; i < parts; i++) {
    if (i) out += SEPARATORS[pick(SEPARATORS.length)];
    if (depth < 2 && pick(4) === 0) {
      const b = pick(3);
      out += WORDS[pick(WORDS.length)] + OPEN[b] + name(pick, depth + 1) + CLOSE[b];
    } else out += WORDS[pick(WORDS.length)];
  }
  return out;
}

const balanced = (text) => {
  const stack = [];
  for (const c of text) {
    if (OPEN.includes(c)) stack.push(CLOSE[OPEN.indexOf(c)]);
    else if (CLOSE.includes(c) && stack.pop() !== c) return false;
  }
  return !stack.length;
};
const startsUnit = (full, at) =>
  at === 0 || SEPARATORS.includes(full[at - 1]) || OPEN.includes(full[at - 1]) || CLOSE.includes(full[at - 1]) || OPEN.includes(full[at]) || SEPARATORS.includes(full[at]);
const endsUnit = (full, at) => at === full.length || CLOSE.includes(full[at]) || startsUnit(full, at);

test("every shortened label cuts whole units once, keeps brackets balanced and stays distinct", () => {
  for (let seed = 1; seed <= 400; seed++) {
    const pick = random(seed);
    const names = [...new Set(Array.from({ length: 2 + pick(6) }, () => name(pick)))];
    const forms = labelForms(names);
    names.forEach((full, i) => {
      for (const form of forms[i]) {
        const context = `seed ${seed}: ${form} of ${full} among ${JSON.stringify(names)}`;
        assert.equal(form.split("…").length, 2, context);
        assert.ok(balanced(form), context);
        const at = form.indexOf("…"), left = form.slice(0, at), right = form.slice(at + 1);
        const closing = right !== "" && [...right].every((c) => CLOSE.includes(c));
        assert.ok(full.startsWith(left) && startsUnit(full, left.length), context);
        if (!closing) assert.ok(full.endsWith(right) && left.length + right.length < full.length && endsUnit(full, full.length - right.length), context);
        names.forEach((other, j) => {
          if (j === i) return;
          assert.notEqual(form, other, context);
          assert.ok(!forms[j].includes(form), `${context} is also a form of ${other}`);
        });
      }
      assert.deepEqual(forms[i], [...forms[i]].sort((a, b) => b.length - a.length), `seed ${seed}: longest first`);
    });
  }
});

test("a bracketed part keeps its brackets when its inside is cut", () => {
  const [novel, poems, essays] = labelForms(["日本文学（小説・物語）", "日本文学（詩歌）", "日本文学（評論・エッセイ・随筆）"]);
  assert.deepEqual(novel, ["日本文学（小説…）", "…（小説・物語）"]);
  assert.deepEqual(poems, ["…（詩歌）"]);
  assert.ok(essays.includes("日本文学（評論…）"));
});
