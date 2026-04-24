import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { parseExistingToml, processAssets } from "../lib/process.mjs";

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
