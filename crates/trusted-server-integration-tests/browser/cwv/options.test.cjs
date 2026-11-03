const test = require("node:test");
const assert = require("node:assert/strict");
const { parseOptions, conditions } = require("./options.cjs");
const required = [
  "--loading",
  "loading",
  "--combined",
  "combined",
  "--out",
  "out",
  "--correctness-only",
];

test("existing CI defaults keep a 750ms hold and cold unthrottled contexts", () => {
  const options = parseOptions(required);
  assert.equal(options.runs, 5);
  assert.deepEqual(conditions(options), {
    runtimeDelayMs: 750,
    cpuRate: 1,
    cache: "cold",
    network: {
      offline: false,
      latency: 0,
      downloadThroughput: -1,
      uploadThroughput: -1,
    },
  });
});

test("zero delay, warm cache and explicit CPU/network conditions are retained", () => {
  const options = parseOptions([
    ...required,
    "--runtime-delay-ms",
    "0",
    "--cache",
    "warm",
    "--cpu-rate",
    "4",
    "--network-latency-ms",
    "150.5",
    "--download-kbps",
    "1600",
    "--upload-kbps",
    "750",
  ]);
  assert.deepEqual(conditions(options), {
    runtimeDelayMs: 0,
    cpuRate: 4,
    cache: "warm",
    network: {
      offline: false,
      latency: 150.5,
      downloadThroughput: 200000,
      uploadThroughput: 93750,
    },
  });
});

for (const [flag, value] of [
  ["runs", "0"],
  ["runs", "1.5"],
  ["runs", "9007199254740992"],
  ["runtime-delay-ms", "-1"],
  ["runtime-delay-ms", "2147483648"],
  ["runtime-delay-ms", "NaN"],
  ["cpu-rate", "0.5"],
  ["cpu-rate", "Infinity"],
  ["network-latency-ms", "-1"],
  ["download-kbps", ""],
  ["upload-kbps", " "],
  ["upload-kbps", "-1"],
  ["download-kbps", "1e308"],
  ["cache", "hot"],
])
  test(`rejects invalid --${flag} ${JSON.stringify(value)}`, () => {
    assert.throws(() => parseOptions([...required, `--${flag}`, value]));
  });

test("requires explicit measurement inputs and rejects ambiguous modes/unknown options", () => {
  assert.throws(() => parseOptions([]));
  assert.throws(() => parseOptions(required.slice(0, -1)), /baseline/);
  assert.throws(() => parseOptions([...required, "--regressions-only"]));
  assert.throws(() => parseOptions([...required, "--typo"]));
  assert.equal(
    parseOptions([...required.slice(0, -1), "--baseline", "baseline"]).baseline,
    "baseline",
  );
});
