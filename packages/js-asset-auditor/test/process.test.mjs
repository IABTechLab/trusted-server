import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  formatTomlEntry,
  parseExistingToml,
  processAssets,
} from "../lib/process.mjs";

test("processAssets skips non-http script URLs", () => {
  const result = processAssets(
    {
      networkUrls: [
        "data:text/javascript,console.log(1)",
        "blob:https://example.com/abc-123",
        "https://cdn.vendor.test/loader.js",
      ],
      headUrls: [
        "data:text/javascript,console.log(1)",
        "blob:https://example.com/abc-123",
        "https://cdn.vendor.test/loader.js",
      ],
    },
    {
      domain: "publisher.com",
      target: "https://www.publisher.com",
      output: "js-assets.toml",
      diff: false,
      firstParty: [],
      noFilter: true,
    },
  );

  assert.equal(result.summary.surfaced, 1);
  assert.match(result.toml, /origin_url = "https:\/\/cdn\.vendor\.test\/loader\.js"/);
  assert.doesNotMatch(result.toml, /origin_url = "null/);
  assert.doesNotMatch(result.toml, /blob:/);
  assert.doesNotMatch(result.toml, /https:\/\/example\.comhttps:\/\/example\.com/);
});

test("processAssets marks wildcarded assets as head-injected when any original is in head", () => {
  const result = processAssets(
    {
      networkUrls: [
        "https://cdn.vendor.test/prod/1.19.8/raven.js",
        "https://cdn.vendor.test/prod/1.19.9/raven.js",
      ],
      headUrls: ["https://cdn.vendor.test/prod/1.19.9/raven.js"],
    },
    {
      domain: "publisher.com",
      target: "https://www.publisher.com",
      output: "js-assets.toml",
      diff: false,
      firstParty: [],
      noFilter: true,
    },
  );

  assert.equal(result.summary.surfaced, 1);
  assert.match(result.toml, /origin_url = "https:\/\/cdn\.vendor\.test\/prod\/\*\/raven\.js"/);
  assert.equal(result.summary.assets[0].injectInHead, true);
  assert.match(result.toml, /inject_in_head = true/);
});

test("processAssets leaves wildcarded assets body-only when no original is in head", () => {
  const result = processAssets(
    {
      networkUrls: [
        "https://cdn.vendor.test/prod/1.19.8/raven.js",
        "https://cdn.vendor.test/prod/1.19.9/raven.js",
      ],
      headUrls: [],
    },
    {
      domain: "publisher.com",
      target: "https://www.publisher.com",
      output: "js-assets.toml",
      diff: false,
      firstParty: [],
      noFilter: true,
    },
  );

  assert.equal(result.summary.surfaced, 1);
  assert.equal(result.summary.assets[0].injectInHead, false);
  assert.match(result.toml, /inject_in_head = false/);
});

test("formatTomlEntry escapes TOML strings", () => {
  const toml = formatTomlEntry({
    slug: 'prefix:quote"slash\\asset',
    path: '/js-assets/prefix/quote"slash\\asset.js',
    originUrl: 'https://cdn.example.com/quote"slash\\asset.js',
    injectInHead: true,
    hasWildcard: false,
  });

  assert.match(toml, /slug = "prefix:quote\\"slash\\\\asset"/);
  assert.match(toml, /path = "\/js-assets\/prefix\/quote\\"slash\\\\asset\.js"/);
  assert.match(
    toml,
    /origin_url = "https:\/\/cdn\.example\.com\/quote\\"slash\\\\asset\.js"/,
  );
});

test("parseExistingToml includes commented diff suggestions", () => {
  const content = `[[js_assets]]
slug = "live:asset"
origin_url = "https://cdn.example.com/live.js"

# [[js_assets]]
# slug = "pending:asset"
# origin_url = "https://cdn.example.com/pending.js"
`;

  assert.deepEqual(parseExistingToml(content), [
    {
      slug: "live:asset",
      originUrl: "https://cdn.example.com/live.js",
    },
    {
      slug: "pending:asset",
      originUrl: "https://cdn.example.com/pending.js",
    },
  ]);
});

test("processAssets diff mode is idempotent across repeated runs", () => {
  const outputDir = mkdtempSync(join(tmpdir(), "js-asset-auditor-"));
  const outputPath = join(outputDir, "js-assets.toml");

  writeFileSync(
    outputPath,
    `[[js_assets]]
slug = "existing:asset"
path = "/js-assets/existing/loader.js"
origin_url = "https://cdn.example.com/existing.js"
inject_in_head = true
`,
  );

  const input = {
    networkUrls: [
      "https://cdn.example.com/existing.js",
      "https://cdn.vendor.test/new-loader.js",
    ],
    headUrls: [
      "https://cdn.example.com/existing.js",
      "https://cdn.vendor.test/new-loader.js",
    ],
  };

  const args = {
    domain: "publisher.com",
    target: "https://www.publisher.com",
    output: outputPath,
    diff: true,
    firstParty: [],
    noFilter: true,
  };

  const firstRun = processAssets(input, args);
  assert.equal(firstRun.summary.new.length, 1);
  writeFileSync(outputPath, firstRun.toml);

  const secondRun = processAssets(input, args);
  assert.equal(secondRun.summary.new.length, 0);
  assert.equal(secondRun.summary.confirmed.length, 2);
  assert.equal(secondRun.summary.missing.length, 0);
  assert.equal(
    readFileSync(outputPath, "utf8").match(/# \[\[js_assets\]\]/g)?.length,
    1,
  );
});
