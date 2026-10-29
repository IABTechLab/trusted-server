const { test } = require("node:test");
const assert = require("node:assert/strict");
const { sessionCls, distribution } = require("./metrics.cjs");
const shift = (startTime, value, hadRecentInput = false) => ({
  startTime,
  value,
  hadRecentInput,
});

test("CLS excludes recent input and selects the largest session, not the total", () => {
  assert.equal(
    sessionCls([
      shift(0, 0.1),
      shift(500, 0.2),
      shift(600, 9, true),
      shift(1600, 0.4),
    ]),
    0.4,
  );
  assert.equal(sessionCls([]), 0);
});
test("CLS starts a new session at the one-second gap", () => {
  assert.equal(
    sessionCls([shift(0, 0.1), shift(999, 0.2)]),
    0.30000000000000004,
  );
  assert.equal(sessionCls([shift(0, 0.1), shift(1000, 0.2)]), 0.2);
});
test("CLS session cannot extend beyond five seconds even with short gaps", () => {
  const entries = Array.from({ length: 7 }, (_, i) => shift(i * 900, 0.1));
  assert(Math.abs(sessionCls(entries) - 0.6) < 1e-10);
});
test("recent input does not bridge otherwise separated sessions", () => {
  assert.equal(
    sessionCls([shift(0, 0.1), shift(800, 9, true), shift(1500, 0.2)]),
    0.2,
  );
});
test("distribution retains all finite samples, medians and ranges", () => {
  assert.deepEqual(distribution([3, 1, 7, 5]), {
    count: 4,
    median: 4,
    min: 1,
    max: 7,
  });
  assert.equal(distribution([3, 1, 2]).median, 2);
  assert.deepEqual(distribution([null, undefined, NaN]), {
    count: 0,
    median: null,
    min: null,
    max: null,
  });
});
