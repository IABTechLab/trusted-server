const assert = require("node:assert/strict");
const { createHash } = require("node:crypto");
const { readFileSync, readdirSync } = require("node:fs");
const { resolve, join } = require("node:path");

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
// allow-domain: securepubads.g.doubleclick.net
const GPT_SCRIPT = "https://securepubads.g.doubleclick.net/tag/js/gpt.js";
const CANDIDATE_KEYS = [
  "hb_pb",
  "hb_bidder",
  "hb_adid",
  "hb_cache_host",
  "hb_cache_path",
];

function inventory(input) {
  assert.equal(input.schemaVersion, 1, "inventory schemaVersion must be 1");
  assert.equal(
    input.dedicatedTestInventory,
    true,
    "dedicated test inventory is required",
  );
  assert.equal(
    input.authorizeGoogleTraffic,
    true,
    "explicit Google traffic authorization is required",
  );
  const path = (value) => {
    assert.match(
      value ?? "",
      /^\/[0-9]+\/[A-Za-z0-9_./-]+$/,
      "invalid dedicated GAM ad unit path",
    );
    assert.ok(!value.includes(".."), "GAM paths cannot contain traversal");
    return value;
  };
  const registered = input.registered;
  assert.ok(
    registered && input.unrelated,
    "registered and unrelated inventory are required",
  );
  path(registered.path);
  path(input.unrelated.path);
  assert.notEqual(
    registered.path,
    input.unrelated.path,
    "distinct paths are required for unambiguous network correlation",
  );
  for (const key of ["tsCreativeId", "ordinaryCreativeId"]) {
    assert.ok(
      Number.isSafeInteger(registered[key]) && registered[key] > 0,
      `${key} must be a positive GAM creative ID`,
    );
  }
  assert.notEqual(
    registered.tsCreativeId,
    registered.ordinaryCreativeId,
    "TS and ordinary creative IDs must differ",
  );
  for (const key of ["tsMarker", "ordinaryMarker"]) {
    assert.ok(
      typeof registered[key] === "string" && registered[key].trim(),
      `${key} must be a visible creative text marker`,
    );
  }
  assert.notEqual(
    registered.tsMarker,
    registered.ordinaryMarker,
    "creative markers must differ",
  );
  return {
    schemaVersion: 1,
    dedicatedTestInventory: true,
    authorizeGoogleTraffic: true,
    registered: { ...registered },
    unrelated: { path: input.unrelated.path },
    gptScriptUrl: GPT_SCRIPT,
  };
}

// Canonical locale-independent launcher order: cd DIST; find . -maxdepth 1
// -name 'tsjs-*.js' -type f | LC_ALL=C sort; sha256sum each './filename',
// then sha256sum those output bytes. Bundle filenames are ASCII.
function distFingerprint(directory) {
  const files = readdirSync(directory, { withFileTypes: true })
    .filter((entry) => entry.isFile() && /^tsjs-.*\.js$/.test(entry.name))
    .map((entry) => entry.name)
    .sort();
  assert.ok(files.length, "no TSJS bundles found");
  return sha256(
    files
      .map(
        (file) => `${sha256(readFileSync(join(directory, file)))}  ./${file}\n`,
      )
      .join(""),
  );
}

function loadInventory(file) {
  assert.ok(
    file,
    "--inventory FILE is required; no live-inventory defaults or skips exist",
  );
  return inventory(JSON.parse(readFileSync(resolve(file), "utf8")));
}

function options(argv) {
  const result = {};
  for (let index = 0; index < argv.length; index++) {
    const flag = argv[index];
    assert.ok(
      [
        "--inventory",
        "--dist",
        "--wasm",
        "--prebid",
        "--image",
        "--out",
        "--origin-port",
        "--baseline-wasm",
        "--baseline-dist",
        "--prepare",
        "--list",
        "--preflight",
      ].includes(flag),
      `unknown option ${flag}`,
    );
    const key = flag.slice(2);
    assert.ok(!(key in result), `duplicate option ${flag}`);
    if (["prepare", "list", "preflight"].includes(key)) result[key] = true;
    else {
      assert.ok(
        argv[index + 1] && !argv[index + 1].startsWith("--"),
        `missing value for ${flag}`,
      );
      result[key] = argv[++index];
    }
  }
  assert.equal(
    Boolean(result["baseline-wasm"]),
    Boolean(result["baseline-dist"]),
    "--baseline-wasm and --baseline-dist must be supplied together",
  );
  loadInventory(result.inventory);
  if (!result.list && !result.preflight) {
    for (const key of ["dist", "prebid", "out"])
      assert.ok(result[key], `--${key} is required`);
  }
  if (result["origin-port"] !== undefined) {
    assert.match(result["origin-port"], /^\d+$/);
    const port = Number(result["origin-port"]);
    assert.ok(port > 0 && port <= 65535, "invalid origin port");
  }
  return result;
}

function targeting(raw) {
  const result = {};
  for (const [key, value] of new URLSearchParams(raw)) {
    assert.ok(!(key in result), `ambiguous duplicate targeting ${key}`);
    result[key] = value.split(",").sort();
  }
  return result;
}

function decodeAdRequest(request, configuredPaths) {
  const url = new URL(request.url);
  assert.ok(url.pathname.endsWith("/gampad/ads"), "not a GPT ad request");
  const params = new URLSearchParams(url.search);
  if (request.method === "POST") {
    assert.ok(
      request.postData && !request.postData.trimStart().startsWith("{"),
      "undecodable GPT POST body",
    );
    for (const [key, value] of new URLSearchParams(request.postData)) {
      assert.ok(!params.has(key), `ambiguous query/body parameter ${key}`);
      params.append(key, value);
    }
  } else assert.equal(request.method, "GET", "unsupported GPT request method");
  for (const key of new Set(params.keys()))
    assert.equal(
      params.getAll(key).length,
      1,
      `duplicate GPT parameter ${key}`,
    );
  const global = targeting(params.get("cust_params") || "");
  let paths;
  let perSlot;
  if (params.has("enc_prev_ius")) {
    assert.ok(!params.has("iu"), "ambiguous single/SRA inventory");
    const parts = (params.get("iu_parts") || "").split(",");
    assert.ok(parts.every(Boolean), "missing SRA inventory parts");
    paths = params
      .get("enc_prev_ius")
      .split(",")
      .map((encoded) => {
        assert.match(
          encoded,
          /^\/?\d+(?:\/\d+)*$/,
          "undecodable SRA inventory indexes",
        );
        const indexes = encoded.replace(/^\//, "").split("/").map(Number);
        assert.ok(
          indexes.every((index) => index < parts.length),
          "SRA inventory index out of bounds",
        );
        return "/" + indexes.map((index) => parts[index]).join("/");
      });
    perSlot = params.has("prev_scp")
      ? params.get("prev_scp").split("|")
      : paths.map(() => "");
    assert.equal(
      perSlot.length,
      paths.length,
      "SRA targeting/slot count mismatch",
    );
  } else {
    assert.ok(params.get("iu"), "undecodable GPT request without inventory");
    paths = [params.get("iu")];
    perSlot = [params.get("scp") || ""];
  }
  assert.equal(
    new Set(paths).size,
    paths.length,
    "repeated GAM path is ambiguous in one request",
  );
  return paths.map((path, index) => {
    assert.ok(
      configuredPaths.includes(path),
      `unexpected or undecodable inventory ${path}`,
    );
    return {
      path,
      targeting: { ...global, ...targeting(perSlot[index]) },
      requestIndex: request.index,
    };
  });
}

function candidateTargeting(value) {
  return Object.fromEntries(
    Object.entries(value)
      .filter(([key]) => key.startsWith("hb_") || key === "ts_initial")
      .map(([key, values]) => [key, [...values].sort()]),
  );
}

// One public slotRequested event must account for every decoded per-slot request.
// This deliberately rejects unrecognised GPT encodings rather than guessing.
function assertEvidence({ requests, events, expected, paths }) {
  const decoded = requests.flatMap((request) =>
    decodeAdRequest(request, paths),
  );
  const requested = events.filter((event) => event.type === "slotRequested");
  assert.equal(
    decoded.length,
    requested.length,
    "network/public request count mismatch",
  );
  for (const path of paths) {
    const actual = decoded.filter((entry) => entry.path === path);
    const publicEvents = requested.filter((entry) => entry.path === path);
    const wanted = expected.filter((entry) => entry.path === path);
    assert.equal(
      actual.length,
      wanted.length,
      `extra or missing real request for ${path}`,
    );
    assert.equal(
      publicEvents.length,
      wanted.length,
      `extra or missing slotRequested for ${path}`,
    );
    const renders = events.filter(
      (event) => event.type === "slotRenderEnded" && event.path === path,
    );
    assert.equal(renders.length, wanted.length, `render count for ${path}`);
    for (let index = 0; index < wanted.length; index++) {
      const expectation = wanted[index];
      const native = publicEvents[index];
      assert.equal(
        native.id,
        expectation.id,
        "request belongs to wrong final mount",
      );
      assert.deepEqual(
        candidateTargeting(actual[index].targeting),
        candidateTargeting(native.targeting),
        "network targeting disagrees with public lifecycle",
      );
      if (expectation.candidate) {
        for (const key of ["hb_pb", "hb_bidder", "hb_adid"])
          assert.ok(
            actual[index].targeting[key]?.length,
            `first eligible request lacks ${key}`,
          );
        assert.deepEqual(actual[index].targeting.hb_bidder, ["fixture-bidder"]);
        assert.deepEqual(actual[index].targeting.hb_adid, [
          "fixture-win-fixture-header",
        ]);
      } else {
        for (const key of CANDIDATE_KEYS)
          assert.ok(
            !actual[index].targeting[key]?.length,
            `ordinary request retained ${key}`,
          );
      }
      assert.equal(
        renders[index].id,
        expectation.id,
        "render belongs to wrong mount",
      );
      assert.equal(
        renders[index].isEmpty,
        false,
        "test inventory returned no fill",
      );
      assert.equal(
        renders[index].creativeId,
        expectation.creativeId,
        "wrong GAM creative",
      );
      assert.ok(
        expectation.markerObserved,
        "expected creative text marker was not observed",
      );
    }
  }
  return { decoded, publicRequestCount: requested.length };
}

function appConfig(template, input, prebid, originPort, legacy = false) {
  const config = inventory(input);
  assert.match(prebid.sha256, /^[a-f0-9]{64}$/);
  assert.match(prebid.sri, /^sha384-[A-Za-z0-9+/=]+$/);
  let text = template.split("[[creative_opportunities.slot]]")[0];
  text = text
    .replace(
      'origin_url = "http://127.0.0.1:8888"',
      `origin_url = "http://127.0.0.1:${originPort}"`,
    )
    .replace(
      '"ads.example.com"',
      JSON.stringify(new URL(config.gptScriptUrl).hostname),
    )
    .replace(
      'script_url = "https://ads.example.com/gpt.js"',
      `script_url = ${JSON.stringify(config.gptScriptUrl)}`,
    )
    .replace("rewrite_script = false", "rewrite_script = true")
    .replace(
      'external_bundle_url = "https://assets.example.com/fixture-prebid.js"',
      `external_bundle_url = "https://assets.example.com/fixture-prebid.js"\nexternal_bundle_sha256 = "${prebid.sha256}"\nexternal_bundle_sri = "${prebid.sri}"`,
    )
    .replace(
      'gam_network_id = "123456789"',
      `gam_network_id = ${JSON.stringify(config.registered.path.split("/")[1])}`,
    );
  text += `[[creative_opportunities.slot]]\nid = "fixture-header"\n${legacy ? "" : 'initialization = "publisher_registered"\n'}gam_unit_path = ${JSON.stringify(config.registered.path)}\ndiv_id = "${legacy ? "real-gpt-ssr" : "real-gpt-"}"\npage_patterns = ["/publisher-registration-real-gpt", "/publisher-registration-real-gpt/*"]\nformats = [{ width = 300, height = 250 }${legacy ? ", { width = 300, height = 600 }" : ""}]\n\n[creative_opportunities.slot.providers.prebid.bidders.fixture-bidder]\nplacement = "fixture-header"\n\n[debug]\ninject_adm_for_testing = false\n`;
  assert.ok(text.includes("rewrite_script = true"));
  assert.ok(
    !text.includes("ads.example.com"),
    "fake GPT backend must not appear in dedicated config",
  );
  return text;
}

function assertBaselineEvidence({
  requests,
  events,
  paths,
  liveSlots,
  registered,
  initialMarkerObserved,
  finalMarkerObserved,
}) {
  const decoded = requests.flatMap((request) =>
    decodeAdRequest(request, paths),
  );
  const requested = events.filter((event) => event.type === "slotRequested");
  assert.equal(
    decoded.length,
    requested.length,
    "baseline network/public count mismatch",
  );
  assert.equal(
    decoded.filter((slot) => slot.path === registered.path).length,
    2,
    "baseline requires exactly original and final real request memberships",
  );
  assert.equal(
    decoded.filter((slot) => slot.path !== registered.path).length,
    0,
    "unrelated baseline inventory was requested",
  );
  const members = requested.filter((event) => event.path === registered.path);
  assert.deepEqual(
    members.map((event) => event.id),
    ["real-gpt-ssr", "real-gpt-baseline-final"],
  );
  for (let index = 0; index < decoded.length; index++)
    assert.deepEqual(
      candidateTargeting(decoded[index].targeting),
      candidateTargeting(members[index].targeting),
      "baseline network/lifecycle targeting mismatch",
    );
  const renders = events.filter(
    (event) =>
      event.type === "slotRenderEnded" && event.path === registered.path,
  );
  assert.equal(
    renders.length,
    2,
    "baseline requires explicit public completion, not a no-render timeout",
  );
  assert.equal(renders[0].id, "real-gpt-ssr");
  assert.equal(
    renders[0].creativeId,
    registered.tsCreativeId,
    "original SSR must render known TS creative before replacement",
  );
  assert.equal(renders[0].isEmpty, false);
  assert.equal(
    initialMarkerObserved,
    true,
    "original SSR marker must have actually rendered",
  );
  assert.equal(renders[1].id, "real-gpt-baseline-final");
  if (!renders[1].isEmpty) {
    assert.ok(
      [registered.tsCreativeId, registered.ordinaryCreativeId].includes(
        renders[1].creativeId,
      ),
      "unknown creative is inventory failure, not original-issue proof",
    );
    assert.equal(
      finalMarkerObserved,
      true,
      "nonempty final creative needs its configured marker",
    );
  }
  const matching = liveSlots.filter((slot) => slot.path === registered.path);
  const duplicatedMembership =
    matching.some((slot) => slot.id === "real-gpt-ssr" && !slot.connected) &&
    matching.some(
      (slot) => slot.id === "real-gpt-baseline-final" && slot.connected,
    );
  const lostFinalTsCreative =
    renders[1].isEmpty ||
    renders[1].creativeId === registered.ordinaryCreativeId;
  assert.ok(
    duplicatedMembership || lostFinalTsCreative,
    "original failure was not reproduced by actual real GPT evidence",
  );
  return {
    originalFailureObserved: true,
    duplicatedMembership,
    lostFinalTsCreative,
    decoded,
    renders,
    liveSlots,
  };
}

module.exports = {
  GPT_SCRIPT,
  sha256,
  distFingerprint,
  inventory,
  loadInventory,
  options,
  decodeAdRequest,
  assertEvidence,
  assertBaselineEvidence,
  appConfig,
};
