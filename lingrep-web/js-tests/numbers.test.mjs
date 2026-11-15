import test from "node:test";
import assert from "node:assert/strict";
import { count, fixed, places, significant } from "../assets/numbers.js";

test("a count is grouped in threes", () => {
  assert.equal(count(1234567), "1,234,567");
  assert.equal(count("72730"), "72,730");
  assert.equal(count(0), "0");
});

test("a fixed number keeps its fraction digits", () => {
  assert.equal(fixed(1234.5, 2), "1,234.50");
  assert.equal(fixed(0.04, 1), "0.0");
  assert.equal(fixed(7, 0), "7");
});

test("numbers are rounded to at most three significant digits", () => {
  assert.equal(significant(1.23456), "1.23");
  assert.equal(significant(2), "2");
  assert.equal(significant(1234.5), "1,230");
});

test("a column's fraction digits follow its smallest positive value", () => {
  assert.equal(places([120, 15]), 0);
  assert.equal(places([120, 15, 3]), 1);
  assert.equal(places([0.5, 250]), 2);
  assert.equal(places([0.0004, 250]), 2);
  assert.equal(places([0, 0]), 0);
  assert.equal(places([]), 0);
});
