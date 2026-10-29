const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const test = require("node:test");
const { JSDOM } = require("jsdom");
const {
  inlineJson,
  documentPage,
  payload,
  creative,
  pucPage,
} = require("./pages.cjs");
const source = fs.readFileSync(require.resolve("./gpt-fixture.js"), "utf8");

function fixture(initialLoad = "disabled") {
  const elements = new Map(
    ["example-header-final", "unrelated"].map((id) => [
      id,
      { id, isConnected: true },
    ]),
  );
  const window = {
    publisherFixtureOptions: { initialLoad },
    addEventListener() {},
  };
  vm.runInNewContext(source, {
    window,
    document: { getElementById: (id) => elements.get(id) },
    performance: { now: () => 0 },
  });
  const g = window.googletag;
  const slot = g
    .defineSlot("/123/example-placement", [[300, 250]], "example-header-final")
    .addService(g.pubads());
  const unrelated = g
    .defineSlot("/123/unrelated", [[300, 250]], "unrelated")
    .addService(g.pubads());
  g.pubads().enableSingleRequest();
  g.enableServices();
  return { g, f: window.fixture, window, slot, unrelated };
}

test("inline JSON cannot terminate its script, including line separators", () => {
  const input = {
    id: '</script><script>throw new Error("injected")</script>&\u2028\u2029',
  };
  assert.deepEqual(JSON.parse(inlineJson(input)), input);
  assert(!/[<>&\u2028\u2029]/u.test(inlineJson(input)));
  for (const id of [
    input.id,
    '</SCRIPT><SCRIPT>throw new Error("injected")</SCRIPT>',
  ]) {
    const html = documentPage({ id });
    assert.equal((html.match(/<script>/gi) ?? []).length, 1);
    assert.equal((html.match(/<\/script>/gi) ?? []).length, 1);
  }
});

test("creative labels remain text instead of inserting inline markup", () => {
  for (const tag of ["script", "SCRIPT"]) {
    const html = creative(
      `</div><${tag}>throw new Error("injected")</${tag}>&`,
    );
    assert(html.includes(`&lt;/div&gt;&lt;${tag}&gt;`));
    assert.equal((html.match(/<script>/gi) ?? []).length, 1);
    assert.equal((html.match(/<\/script>/gi) ?? []).length, 1);
  }
});

test("Universal Creative uses the supplied publisher origin without inline injection", () => {
  const origin =
    "http://publisher.example.com:8000/</script><script>injected</script>&";
  const html = pucPage(origin);
  assert.equal((html.match(/<script/gi) ?? []).length, 2);
  assert.equal((html.match(/<\/script>/gi) ?? []).length, 2);
  assert(html.includes(inlineJson(origin)));
  assert(pucPage().includes('"https://publisher.example.com"'));
});

test("nested Universal Creative marker sources map to the final publisher element", () => {
  let message;
  const child = { length: 0, frames: [] };
  const root = { length: 1, frames: [child] };
  const element = { id: "publisher-final-header" };
  const frame = {
    contentWindow: root,
    parentElement: element,
    dataset: { request: "2" },
  };
  const window = {
    addEventListener(name, callback) {
      if (name === "message") message = callback;
    },
  };
  vm.runInNewContext(source, {
    window,
    document: { querySelectorAll: () => [frame] },
    performance: { now: () => 0 },
  });
  message({
    data: { type: "fixture-creative-ready", label: "Server result" },
    source: child,
  });
  assert.equal(window.fixture.creatives[0].index, 2);
  assert.equal(window.fixture.creatives[0].label, "Server result");
  assert.equal(
    window.fixture.elements[window.fixture.creatives[0].element],
    element,
  );
});

test("native creative iframe excludes default browser border and inline baseline space", () => {
  const dom = new JSDOM('<div id="example-header-final"></div>');
  const { window } = dom;
  window.publisherFixtureOptions = { initialLoad: "disabled" };
  vm.runInNewContext(source, {
    window,
    document: window.document,
    performance: window.performance,
  });
  const g = window.googletag;
  const slot = g
    .defineSlot("/123/example-placement", [[300, 250]], "example-header-final")
    .addService(g.pubads());
  g.display("example-header-final");
  g.pubads().refresh([slot]);
  window.fixture.render(0);
  const frame = window.document.querySelector("#example-header-final > iframe");
  assert.equal(frame.width, "300");
  assert.equal(frame.height, "250");
  assert.equal(window.getComputedStyle(frame).display, "block");
  assert.equal(
    Number.parseFloat(window.getComputedStyle(frame).borderTopWidth),
    0,
  );
  assert.equal(window.fixture.requests.length, 1);
  dom.window.close();
});

test("payload is fictional offline data with explicit mode and candidate dimensions", () => {
  assert.equal(payload().slots[0].initialization, "publisher_registered");
  assert(!("initialization" in payload({ registered: false }).slots[0]));
  assert.equal(payload({ two: true }).slots.length, 2);
  assert(!("w" in payload({ candidate: "unknown" }).bids.header));
});

test("undisplayed refresh records its invocation but cannot request a defined slot", () => {
  const { g, f, slot } = fixture();
  g.pubads().refresh([slot]);
  assert.equal(f.requests.length, 0);
  const calls = f.trace.filter((entry) => entry.event === "nativeRefresh");
  assert.equal(calls.length, 1);
  assert.equal(calls[0].natives[0], f.nativeSlots.indexOf(slot));
  assert.equal(calls[0].scoped, true);
  g.display("example-header-final");
  g.pubads().refresh([slot]);
  assert.equal(f.requests.length, 1);
  assert.equal(
    f.trace.filter((entry) => entry.event === "nativeRefresh").length,
    2,
  );
});

test("every displayed native refresh counts even when TS claims the slot", () => {
  const { g, f, window, slot } = fixture();
  window.tsjs = {
    firstImpression: {
      slots: { "example-header-final": { owner: "trusted_server" } },
    },
  };
  g.display("example-header-final");
  g.pubads().refresh([slot]);
  g.pubads().refresh([slot]);
  assert.equal(f.requests.length, 2);
  assert.equal(f.requests[0].native, f.requests[1].native);
  assert.notEqual(f.requests[0].batch, f.requests[1].batch);
});

test("enabled-load SRA display fetches an unrelated unfetched slot", () => {
  const { g, f, unrelated } = fixture("enabled");
  g.display("example-header-final");
  assert.equal(f.requests.length, 2);
  assert(
    f.requests.some(
      (entry) => entry.native === f.nativeSlots.indexOf(unrelated),
    ),
  );
  assert.equal(f.requests[0].batch, f.requests[1].batch);
});

test("disabled-load display and scoped refresh leave unrelated SRA slot untouched", () => {
  const { g, f, slot, unrelated } = fixture();
  g.display("example-header-final");
  assert.equal(f.requests.length, 0);
  g.pubads().refresh([slot], { changeCorrelator: false });
  assert.equal(f.requests.length, 1);
  assert(
    !f.requests.some(
      (entry) => entry.native === f.nativeSlots.indexOf(unrelated),
    ),
  );
  assert.equal(f.requests[0].options.changeCorrelator, false);
});

test("request targeting is copied at invocation, native and element identities retained", () => {
  const { g, f, slot } = fixture();
  slot.setTargeting("example_key", "before");
  g.display("example-header-final");
  g.pubads().refresh([slot]);
  slot.setTargeting("example_key", "after");
  assert.equal(f.requests[0].targeting.example_key[0], "before");
  assert.equal(f.nativeSlots[f.requests[0].native], slot);
  assert.equal(f.elements[f.requests[0].element].id, "example-header-final");
});

test("unknown load policy has no public getter and destruction is recorded before element removal", () => {
  const { g, f, slot } = fixture("unknown");
  assert.equal(g.getConfig, undefined);
  assert.equal(g.pubads().isInitialLoadDisabled, undefined);
  g.destroySlots([slot]);
  assert(!g.pubads().getSlots().includes(slot));
  assert.equal(f.trace.at(-1).event, "nativeDestroy");
  assert.equal(f.trace.at(-1).connected, true);
});
