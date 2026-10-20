import test from "node:test";
import assert from "node:assert/strict";
import { groupCatalog } from "../assets/results-explorer.js";

test("characterizes regrouping one thousand catalog rows", () => {
  const rows = Array.from({ length: 1_000 }, (_, index) => ({
    dataset: { exactTotal: String((index % 17) + 1) },
    querySelectorAll: () => [{ dataset: { groupField: "category", groupKey: `s:g${index % 20}`, groupLabel: `g${index % 20}` } }],
  }));
  const samples = [];
  for (let run = 0; run < 30; run++) {
    const start = performance.now();
    assert.equal(groupCatalog(rows, "category").length, 20);
    samples.push(performance.now() - start);
  }
  samples.sort((a, b) => a - b);
  const p50 = samples[Math.floor(samples.length * 0.50)];
  const p95 = samples[Math.floor(samples.length * 0.95)];
  console.log(`rows=1000 regroup_ms_p50=${p50.toFixed(3)} regroup_ms_p95=${p95.toFixed(3)}`);
});
