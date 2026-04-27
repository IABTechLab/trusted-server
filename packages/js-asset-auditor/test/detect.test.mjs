import assert from "node:assert/strict";
import test from "node:test";

import { detectIntegrations, generateConfig } from "../lib/detect.mjs";

test("detectIntegrations matches GTM script path exactly", () => {
  const detection = detectIntegrations([
    "https://www.googletagmanager.com/gtm.js?id=GTM-TEST",
    "https://www.googletagmanager.com/foo/gtm.js.bak?id=GTM-WRONG",
  ]);

  assert.equal(detection.integrations.length, 1);
  assert.equal(detection.integrations[0].id, "google_tag_manager");
  assert.deepEqual(detection.integrations[0].extracted, {
    container_id: "GTM-TEST",
  });
});

test("detectIntegrations picks up prebid wrapper/load script names", () => {
  const detection = detectIntegrations([
    "https://web.prebidwrapper.com/golf-WnLmpLyEjL/default-v2/prebid-load.js",
    "https://cdn.vendor.test/prebid.min.js",
  ]);

  const prebid = detection.integrations.find((entry) => entry.id === "prebid");
  assert.equal(prebid.todos.includes("server_url"), true);
  assert.equal(prebid.todos.includes("bidders"), true);
});

test("detectIntegrations adds runtime Prebid bidders when available", () => {
  const detection = detectIntegrations(
    ["https://web.prebidwrapper.com/golf-WnLmpLyEjL/default-v2/prebid-load.js"],
    { prebidBidders: ["ix", "kargo", "rubicon"] },
  );

  const prebid = detection.integrations.find((entry) => entry.id === "prebid");
  assert.deepEqual(prebid.extracted.bidders, ["ix", "kargo", "rubicon"]);
  assert.equal(prebid.todos.includes("server_url"), true);
  assert.equal(prebid.todos.includes("bidders"), false);
});

test("generateConfig comments TODO fields so Prebid config stays parseable", () => {
  const detection = detectIntegrations([
    "https://cdn.vendor.test/prebid.min.js",
    "https://aim.loc.kr/identity-lockr-trust-server.js",
  ]);
  const config = generateConfig(
    "publisher.com",
    "https://www.publisher.com",
    detection,
  );

  assert.match(
    config,
    /\[integrations\.prebid\][\s\S]*# server_url = ""  # TODO: set your Prebid Header Bidding server_url/,
  );
  assert.match(
    config,
    /\[integrations\.prebid\][\s\S]*# bidders = ""  # TODO: set your Prebid Header Bidding bidders/,
  );
  assert.doesNotMatch(config, /^bidders = ""/m);
  assert.match(
    config,
    /\[integrations\.lockr\][\s\S]*# app_id = ""  # TODO: set your Lockr Identity app_id/,
  );
});

test("generateConfig writes auto-detected Prebid bidders", () => {
  const detection = detectIntegrations(
    ["https://web.prebidwrapper.com/golf-WnLmpLyEjL/default-v2/prebid-load.js"],
    { prebidBidders: ["ix", "kargo"] },
  );

  const config = generateConfig(
    "publisher.com",
    "https://www.publisher.com",
    detection,
  );

  assert.match(config, /\[integrations\.prebid\]\nenabled = false/);
  assert.match(config, /bidders = \["ix", "kargo"\]  # auto-detected/);
  assert.match(
    config,
    /# server_url = ""  # TODO: set your Prebid Header Bidding server_url/,
  );
  assert.doesNotMatch(config, /# bidders = ""/);
});

test("generateConfig only auto-enables fully configured integrations", () => {
  const config = generateConfig("publisher.com", "https://www.publisher.com", {
    integrations: [
      {
        id: "gpt",
        label: "Google Publisher Tags",
        category: "full",
        extracted: { script_url: "https://securepubads.g.doubleclick.net/tag/js/gpt.js" },
        defaults: {},
        todos: [],
      },
      {
        id: "google_tag_manager",
        label: "Google Tag Manager",
        category: "partial",
        extracted: {},
        defaults: {},
        todos: ["container_id"],
      },
      {
        id: "prebid",
        label: "Prebid Header Bidding",
        category: "detect_only",
        extracted: {},
        defaults: { timeout_ms: 1000 },
        todos: ["server_url", "bidders"],
      },
    ],
  });

  assert.match(config, /\[integrations\.gpt\]\nenabled = true/);
  assert.match(config, /\[integrations\.google_tag_manager\]\nenabled = false/);
  assert.match(config, /\[integrations\.prebid\]\nenabled = false/);
});

test("generateConfig escapes TOML strings safely", () => {
  const config = generateConfig("pub\\domain.com", 'https://example.com/?q="quoted"', {
    integrations: [
      {
        id: "gpt",
        label: "Google Publisher Tags",
        category: "full",
        extracted: {
          script_url: 'https://cdn.example.com/tag/"quoted"\\path.js',
        },
        defaults: {},
        todos: [],
      },
    ],
  });

  assert.match(config, /domain = "pub\\\\domain\.com"/);
  assert.match(
    config,
    /script_url = "https:\/\/cdn\.example\.com\/tag\/\\"quoted\\"\\\\path\.js"  # auto-detected/,
  );
});
