import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import { generateSlug } from "../lib/process.mjs";

const repoRoot = fileURLToPath(new URL("../../..", import.meta.url));

test("scripts/js-asset-slug.mjs reuses the shared slug implementation", () => {
  const publisherDomain = "test-publisher.com";
  const originUrl = "https://vendor.io/sdk/loader.js";

  const direct = generateSlug(publisherDomain, originUrl);
  const fromScript = execFileSync(
    process.execPath,
    ["scripts/js-asset-slug.mjs", publisherDomain, originUrl],
    { cwd: repoRoot, encoding: "utf8" },
  ).trim();

  assert.equal(fromScript, direct);
});
