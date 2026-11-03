const assert = require("node:assert/strict");
const { parseArgs } = require("node:util");

function parseOptions(args = process.argv.slice(2)) {
  const { values } = parseArgs({
    args,
    options: {
      baseline: { type: "string" },
      loading: { type: "string" },
      combined: { type: "string" },
      out: { type: "string" },
      runs: { type: "string", default: "5" },
      "correctness-only": { type: "boolean" },
      "regressions-only": { type: "boolean" },
      "diagnostic-screen": { type: "boolean" },
      "diagnostic-outages": { type: "boolean" },
      "runtime-delay-ms": { type: "string", default: "750" },
      "cpu-rate": { type: "string", default: "1" },
      "network-latency-ms": { type: "string", default: "0" },
      "download-kbps": { type: "string", default: "0" },
      "upload-kbps": { type: "string", default: "0" },
      cache: { type: "string", default: "cold" },
    },
  });
  assert(
    values.loading && values.combined && values.out,
    "Required: --loading CHANGED_UNRESERVED --combined CHANGED_RESERVED --out DIR; --baseline enables four-way measurement",
  );
  assert(
    values.baseline || values["correctness-only"] || values["regressions-only"],
    "Measurement requires --baseline; CI may use --correctness-only",
  );
  assert(
    !(values["correctness-only"] && values["regressions-only"]),
    "Choose only one of --correctness-only and --regressions-only",
  );
  if (values["diagnostic-outages"]) {
    assert(values.baseline, "--diagnostic-outages requires --baseline");
    assert(
      !values["diagnostic-screen"] &&
        !values["correctness-only"] &&
        !values["regressions-only"],
      "--diagnostic-outages cannot select another scope",
    );
  }
  if (values["diagnostic-screen"]) {
    assert(values.baseline, "--diagnostic-screen requires --baseline");
    assert(
      !values["correctness-only"] && !values["regressions-only"],
      "--diagnostic-screen cannot select full-mode correctness or regressions",
    );
  }
  for (const name of [
    "runs",
    "runtime-delay-ms",
    "cpu-rate",
    "network-latency-ms",
    "download-kbps",
    "upload-kbps",
  ]) {
    assert(values[name].trim() !== "", `--${name} must be a number`);
    values[name] = Number(values[name]);
    assert(Number.isFinite(values[name]), `--${name} must be finite`);
  }
  if (values["diagnostic-screen"])
    assert.equal(
      values["runtime-delay-ms"],
      0,
      "--diagnostic-screen requires explicit --runtime-delay-ms 0",
    );
  assert(
    Number.isSafeInteger(values.runs) && values.runs > 0,
    "--runs must be a positive safe integer",
  );
  assert(values["cpu-rate"] >= 1, "--cpu-rate must be at least 1");
  for (const name of [
    "runtime-delay-ms",
    "network-latency-ms",
    "download-kbps",
    "upload-kbps",
  ])
    assert(values[name] >= 0, `--${name} must be nonnegative`);
  // Node timers overflow beyond this limit and silently schedule a 1ms delay.
  assert(
    values["runtime-delay-ms"] <= 2147483647,
    "--runtime-delay-ms exceeds the Node timer range",
  );
  for (const name of ["download-kbps", "upload-kbps"])
    assert(
      Number.isFinite((values[name] * 1000) / 8),
      `--${name} exceeds the CDP throughput range`,
    );
  assert(
    ["cold", "warm"].includes(values.cache),
    "--cache must be cold or warm",
  );
  return values;
}

function conditions(options) {
  return {
    runtimeDelayMs: options["runtime-delay-ms"],
    cpuRate: options["cpu-rate"],
    cache: options.cache,
    network: {
      offline: false,
      latency: options["network-latency-ms"],
      downloadThroughput:
        options["download-kbps"] === 0
          ? -1
          : (options["download-kbps"] * 1000) / 8,
      uploadThroughput:
        options["upload-kbps"] === 0 ? -1 : (options["upload-kbps"] * 1000) / 8,
    },
  };
}

module.exports = { parseOptions, conditions };
