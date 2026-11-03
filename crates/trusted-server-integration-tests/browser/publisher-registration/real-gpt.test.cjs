const assert = require("node:assert/strict");
const { test } = require("node:test");
const { mkdtempSync, writeFileSync, rmSync, readFileSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { join } = require("node:path");
const {
  GPT_SCRIPT,
  inventory,
  options,
  decodeAdRequest,
  assertEvidence,
  assertBaselineEvidence,
  appConfig,
  distFingerprint,
} = require("./real-gpt.cjs");

const input = {
  schemaVersion: 1,
  dedicatedTestInventory: true,
  authorizeGoogleTraffic: true,
  registered: {
    path: "/123456789/registered",
    tsCreativeId: 101,
    ordinaryCreativeId: 102,
    tsMarker: "Fixture publisher registration winner",
    ordinaryMarker: "Ordinary test creative",
  },
  unrelated: { path: "/123456789/unrelated" },
};
const candidate = {
  hb_adid: ["fixture-win-fixture-header"],
  hb_bidder: ["fixture-bidder"],
  hb_pb: ["1.20"],
};
const single = (extra = {}) => ({
  index: 0,
  method: "GET",
  url:
    "https://publisher.example.com/gampad/ads?" +
    new URLSearchParams({
      iu: input.registered.path,
      cust_params:
        "hb_adid=fixture-win-fixture-header&hb_bidder=fixture-bidder&hb_pb=1.20",
      ...extra,
    }),
  postData: null,
});
const events = () => [
  {
    type: "slotRequested",
    path: input.registered.path,
    id: "real-gpt-final",
    targeting: candidate,
  },
  {
    type: "slotRenderEnded",
    path: input.registered.path,
    id: "real-gpt-final",
    creativeId: 101,
    isEmpty: false,
  },
];
const evidence = () => ({
  requests: [single()],
  events: events(),
  paths: [input.registered.path, input.unrelated.path],
  expected: [
    {
      path: input.registered.path,
      id: "real-gpt-final",
      candidate: true,
      creativeId: 101,
      markerObserved: true,
    },
  ],
});

test("inventory has no permissive default and requires distinct dedicated creative evidence", () => {
  assert.deepEqual(inventory(input).registered, input.registered);
  for (const patch of [
    { authorizeGoogleTraffic: false },
    { dedicatedTestInventory: false },
    { schemaVersion: 2 },
    { registered: { ...input.registered, ordinaryCreativeId: 101 } },
    { registered: { ...input.registered, tsMarker: "" } },
    { unrelated: { path: input.registered.path } },
  ]) {
    assert.throws(() => inventory({ ...input, ...patch }));
  }
  assert.throws(() => options([]), /inventory/);
});

test("options reject missing inventory, missing values, unknown/duplicate flags and bad ports", () => {
  const dir = mkdtempSync(join(tmpdir(), "real-gpt-options-"));
  try {
    const file = join(dir, "inventory.json");
    writeFileSync(file, JSON.stringify(input));
    assert.equal(options(["--inventory", file, "--list"]).list, true);
    for (const tail of [
      ["--unknown"],
      ["--out"],
      ["--list", "--list"],
      ["--origin-port", "0", "--list"],
      ["--origin-port", "NaN", "--list"],
      [],
    ]) {
      assert.throws(() => options(["--inventory", file, ...tail]));
    }
  } finally {
    rmSync(dir, { recursive: true });
  }
});

test("decodes single GET and URL encoded POST without inventing slot membership", () => {
  const decoded = decodeAdRequest(single(), [input.registered.path]);
  assert.deepEqual(decoded[0].targeting, candidate);
  const post = {
    index: 1,
    method: "POST",
    url: "https://publisher.example.com/gampad/ads",
    postData: new URL(single().url).search.slice(1),
  };
  assert.deepEqual(
    decodeAdRequest(post, [input.registered.path])[0].targeting,
    candidate,
  );
  assert.throws(
    () => decodeAdRequest({ ...post, postData: "{}" }, []),
    /undecodable/,
  );
});

test("SRA counts every encoded inventory and aligns per-slot targeting", () => {
  const request = {
    index: 0,
    method: "GET",
    url:
      "https://publisher.example.com/gampad/ads?" +
      new URLSearchParams({
        iu_parts: "123456789,registered,unrelated",
        enc_prev_ius: "/0/1,/0/2",
        prev_scp:
          "hb_pb=1.20&hb_bidder=fixture-bidder&hb_adid=fixture-win-fixture-header|publisher_section=fiction",
      }),
  };
  const decoded = decodeAdRequest(request, [
    input.registered.path,
    input.unrelated.path,
  ]);
  assert.equal(decoded.length, 2);
  assert.equal(decoded[1].path, input.unrelated.path);
  assert.deepEqual(decoded[0].targeting, candidate);
  const facts = evidence();
  facts.requests = [request];
  facts.events.push(
    {
      type: "slotRequested",
      path: input.unrelated.path,
      id: "real-gpt-unrelated",
      targeting: { publisher_section: ["fiction"] },
    },
    {
      type: "slotRenderEnded",
      path: input.unrelated.path,
      id: "real-gpt-unrelated",
      creativeId: 102,
      isEmpty: false,
    },
  );
  facts.expected.push({
    path: input.unrelated.path,
    id: "real-gpt-unrelated",
    candidate: false,
    creativeId: 102,
    markerObserved: true,
  });
  assert.equal(assertEvidence(facts).decoded.length, 2);
});

test("rejects undecodable, duplicate, ambiguous and unknown request inventories", () => {
  for (const query of [
    "",
    "iu=/123456789/registered&iu=/123456789/registered",
    "iu=/123456789/other",
    "iu_parts=123456789,registered&enc_prev_ius=/0/9",
    "iu_parts=123456789,registered&enc_prev_ius=/0/1,/0/1",
    "iu_parts=123456789,registered&enc_prev_ius=/0/1&prev_scp=a=1|b=2",
    "iu_parts=123456789,registered&enc_prev_ius=opaque",
  ]) {
    assert.throws(() =>
      decodeAdRequest(
        {
          index: 0,
          method: "GET",
          url: `https://publisher.example.com/gampad/ads?${query}`,
        },
        [input.registered.path],
      ),
    );
  }
});

test("observer accepts only matching real network, public targeting and creative evidence", () => {
  assert.equal(assertEvidence(evidence()).publicRequestCount, 1);
  for (const change of [
    (facts) => facts.requests.push(single()),
    (facts) => facts.events.push(...events()),
    (facts) => (facts.events[1].creativeId = 102),
    (facts) => (facts.events[1].isEmpty = true),
    (facts) => (facts.events[0].id = "removed-placeholder"),
    (facts) => (facts.events[0].targeting = {}),
    (facts) => (facts.expected[0].markerObserved = false),
    (facts) => (facts.expected[0].candidate = false),
    (facts) => (facts.requests[0] = single({ cust_params: "" })),
  ]) {
    const facts = evidence();
    change(facts);
    assert.throws(() => assertEvidence(facts));
  }
});

test("generated dedicated app config retains real proxy/provider and excludes fake GPT and auto slots", () => {
  const template = readFileSync(
    join(
      __dirname,
      "../../fixtures/configs/trusted-server.publisher-registration.toml",
    ),
    "utf8",
  );
  const app = appConfig(
    template,
    input,
    { sha256: "a".repeat(64), sri: "sha384-YWJj" },
    9999,
  );
  assert.match(app, /rewrite_script = true/);
  const setting = (name) => {
    const prefix = `${name} = `;
    return JSON.parse(
      app
        .split("\n")
        .find((line) => line.startsWith(prefix))
        .slice(prefix.length),
    );
  };
  assert.equal(setting("origin_url"), "http://127.0.0.1:9999");
  assert.equal(
    setting("endpoint"),
    "https://auction.example.com/api/fixture-auction",
  );
  assert.equal(setting("script_url"), GPT_SCRIPT);
  assert.deepEqual(setting("allowed_domains"), [
    "localhost",
    "127.0.0.1",
    "auction.example.com",
    "assets.example.com",
    new URL(GPT_SCRIPT).hostname,
  ]);
  assert.match(app, /publisher-registration-real-gpt/);
  assert.match(app, /initialization = "publisher_registered"/);
  assert.doesNotMatch(app, /fixture-auto|debug_bid/);
  assert.equal(
    (app.match(/\[\[creative_opportunities.slot\]\]/g) || []).length,
    1,
  );
  assert.ok(app.includes(`gam_unit_path = "${input.registered.path}"`));
  const legacy = appConfig(
    template,
    input,
    { sha256: "a".repeat(64), sri: "sha384-YWJj" },
    9999,
    true,
  );
  assert.doesNotMatch(legacy, /initialization/);
  assert.match(legacy, /div_id = "real-gpt-ssr"/);
  assert.match(legacy, /height = 600/);
});

test("dist fingerprint uses canonical C order despite inherited en_US.UTF-8 locale", () => {
  const { spawnSync } = require("node:child_process");
  const dir = mkdtempSync(join(tmpdir(), "real-gpt-dist-"));
  try {
    writeFileSync(join(dir, "tsjs-z.js"), "z");
    writeFileSync(join(dir, "tsjs-a.js"), "a");
    writeFileSync(join(dir, "tsjs-gpt.js"), "gpt");
    writeFileSync(join(dir, "tsjs-gpt_diagnostics.js"), "diagnostics");
    writeFileSync(join(dir, "ignored.txt"), "not a bundle");
    const shell = spawnSync(
      "bash",
      [
        "-c",
        "find . -maxdepth 1 -name 'tsjs-*.js' -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum | awk '{print $1}'",
      ],
      {
        cwd: dir,
        encoding: "utf8",
        env: { ...process.env, LANG: "en_US.UTF-8", LC_ALL: "en_US.UTF-8" },
      },
    );
    assert.equal(shell.status, 0);
    assert.equal(distFingerprint(dir), shell.stdout.trim());
  } finally {
    rmSync(dir, { recursive: true });
  }
});

test("original-main observer demands known SSR creative and observed original failure", () => {
  const baseline = () => ({
    requests: [single(), { ...single({ cust_params: "" }), index: 1 }],
    events: [
      { ...events()[0], id: "real-gpt-ssr" },
      { ...events()[1], id: "real-gpt-ssr" },
      {
        type: "slotRequested",
        path: input.registered.path,
        id: "real-gpt-baseline-final",
        targeting: {},
      },
      {
        type: "slotRenderEnded",
        path: input.registered.path,
        id: "real-gpt-baseline-final",
        creativeId: 102,
        isEmpty: false,
      },
    ],
    paths: [input.registered.path, input.unrelated.path],
    registered: input.registered,
    liveSlots: [
      { path: input.registered.path, id: "real-gpt-ssr", connected: false },
      {
        path: input.registered.path,
        id: "real-gpt-baseline-final",
        connected: true,
      },
    ],
    initialMarkerObserved: true,
    finalMarkerObserved: true,
  });
  assert.equal(
    assertBaselineEvidence(baseline()).originalFailureObserved,
    true,
  );
  for (const change of [
    (facts) => facts.requests.push(single()),
    (facts) => (facts.initialMarkerObserved = false),
    (facts) => (facts.events[1].creativeId = 102),
    (facts) => (facts.events[3].creativeId = 999),
    (facts) => (facts.finalMarkerObserved = false),
    (facts) => {
      facts.events[3].creativeId = 101;
      facts.liveSlots.shift();
    },
  ]) {
    const facts = baseline();
    change(facts);
    assert.throws(() => assertBaselineEvidence(facts));
  }
});

test("actual provider control preserves defaults/path no-bid, forces transition, and resets", () => {
  const {
    FixtureAuctionControl,
  } = require("../../fixtures/frameworks/nextjs/app/publisher-registration-real-gpt/provider-control.ts");
  const control = new FixtureAuctionControl();
  assert.deepEqual(control.snapshot(), { noBid: false });
  assert.deepEqual(control.forPath("/publisher-registration"), {
    noBid: false,
    forcedNoBid: false,
    source: "default",
  });
  assert.deepEqual(control.forPath("/publisher-registration/no-bid"), {
    noBid: true,
    forcedNoBid: false,
    source: "path",
  });
  control.set({ noBid: true });
  assert.deepEqual(control.forPath("/publisher-registration-real-gpt"), {
    noBid: true,
    forcedNoBid: true,
    source: "control",
  });
  for (const value of [
    null,
    [],
    {},
    { noBid: "true" },
    { noBid: true, extra: true },
  ])
    assert.throws(() => control.set(value));
  assert.equal(
    control.snapshot().noBid,
    true,
    "invalid control must not mutate mode",
  );
  control.reset();
  assert.deepEqual(control.snapshot(), { noBid: false });
  assert.equal(control.forPath("/publisher-registration").noBid, false);
  assert.equal(control.forPath("/publisher-registration/no-bid").noBid, true);
  control.set({ noBid: false });
  assert.equal(control.forPath("/publisher-registration/no-bid").noBid, true);
});
