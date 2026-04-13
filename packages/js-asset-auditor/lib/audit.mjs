#!/usr/bin/env node

// JS Asset Auditor CLI
//
// Standalone Playwright-based tool that sweeps a publisher page for third-party
// JS assets and generates js-assets.toml entries.
//
// Usage:
//   node packages/js-asset-auditor/lib/audit.mjs https://www.publisher.com [options]
//   audit-js-assets https://www.publisher.com [options]  (when plugin bin/ is in PATH)
//
// Options:
//   --diff              Compare against existing js-assets.toml
//   --settle <ms>       Settle window after page load (default: 6000)
//   --first-party <h>   Additional first-party hosts (comma-separated)
//   --no-filter         Bypass heuristic filtering
//   --headed            Run browser visibly for debugging
//   --output <path>     Output file path (default: js-assets.toml)
//
// Prerequisites:
//   cd packages/js-asset-auditor && npm install && npx playwright install chromium

import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { processAssets } from "./process.mjs";

// ---------------------------------------------------------------------------
// Config reading
// ---------------------------------------------------------------------------

function readPublisherDomain(repoRoot) {
  const content = readFileSync(
    resolve(repoRoot, "trusted-server.toml"),
    "utf-8",
  );
  const lines = content.split("\n");
  let inPublisher = false;
  for (const line of lines) {
    if (/^\[publisher\]/.test(line)) {
      inPublisher = true;
      continue;
    }
    if (/^\[/.test(line)) {
      inPublisher = false;
      continue;
    }
    if (inPublisher) {
      const m = line.match(/^domain\s*=\s*"([^"]+)"/);
      if (m) return m[1];
    }
  }
  throw new Error(
    "Could not find [publisher].domain in trusted-server.toml",
  );
}

// ---------------------------------------------------------------------------
// CLI argument parsing
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const args = {
    url: null,
    domain: null,
    diff: false,
    settle: 6000,
    firstParty: [],
    noFilter: false,
    headed: false,
    output: "js-assets.toml",
  };

  for (let i = 2; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--domain") {
      args.domain = argv[++i];
    } else if (arg === "--diff") {
      args.diff = true;
    } else if (arg === "--settle") {
      args.settle = parseInt(argv[++i], 10);
    } else if (arg === "--first-party") {
      args.firstParty = argv[++i].split(",").filter(Boolean);
    } else if (arg === "--no-filter") {
      args.noFilter = true;
    } else if (arg === "--headed") {
      args.headed = true;
    } else if (arg === "--output") {
      args.output = argv[++i];
    } else if (!arg.startsWith("--") && !args.url) {
      args.url = arg.startsWith("http") ? arg : `https://${arg}`;
    } else {
      console.error(`Unknown argument: ${arg}`);
      process.exit(1);
    }
  }

  if (!args.url) {
    console.error(
      "Usage: audit-js-assets <url> [--diff] [--settle <ms>] [--first-party <hosts>] [--no-filter] [--headed] [--output <path>]",
    );
    process.exit(1);
  }

  return args;
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

export async function main() {
  const args = parseArgs(process.argv);
  const repoRoot = process.cwd();

  // Resolve publisher domain: --domain flag > trusted-server.toml > infer from URL
  let domain = args.domain;
  if (!domain) {
    try {
      domain = readPublisherDomain(repoRoot);
    } catch {
      // No config file — infer from target URL
      try {
        const host = new URL(args.url).hostname;
        domain = host.startsWith("www.") ? host.slice(4) : host;
      } catch {
        domain = args.url;
      }
      console.error(`No trusted-server.toml found, using domain: ${domain}`);
    }
  }

  let chromium;
  try {
    ({ chromium } = await import("playwright"));
  } catch {
    console.error(
      "Playwright not installed. Run:\n  cd packages/js-asset-auditor && npm install",
    );
    process.exit(1);
  }

  console.error(`Launching browser...`);
  let browser;
  try {
    browser = await chromium.launch({ headless: !args.headed });
  } catch (err) {
    if (err.message.includes("Executable doesn't exist")) {
      console.error(
        "Chromium not installed. Run:\n  cd packages/js-asset-auditor && npx playwright install chromium",
      );
      process.exit(1);
    }
    throw err;
  }

  try {
    const context = await browser.newContext();
    const page = await context.newPage();

    const scriptUrls = [];
    page.on("response", (response) => {
      const req = response.request();
      if (req.resourceType() === "script") {
        scriptUrls.push(req.url());
      }
    });

    console.error(`Navigating to ${args.url}...`);
    await page.goto(args.url, { waitUntil: "load", timeout: 30000 });

    console.error(`Waiting ${args.settle}ms for page to settle...`);
    await page.waitForTimeout(args.settle);

    const headScriptUrls = await page.evaluate(() =>
      Array.from(
        document.head.querySelectorAll("script[src]"),
      ).map((s) => s.src),
    );

    console.error(
      `Found ${scriptUrls.length} network scripts, ${headScriptUrls.length} head scripts`,
    );

    await browser.close();

    console.error("Processing assets...");
    const result = processAssets(
      { networkUrls: scriptUrls, headUrls: headScriptUrls },
      {
        domain,
        target: args.url,
        output: args.output,
        diff: args.diff,
        firstParty: args.firstParty,
        noFilter: args.noFilter,
      },
    );

    if (result.error) {
      console.error(result.error);
      process.exit(1);
    }

    writeFileSync(args.output, result.toml);
    const count =
      result.summary.mode === "init"
        ? result.summary.surfaced
        : result.summary.new.length;
    console.error(`Wrote ${args.output} (${count} entries)`);

    console.log(JSON.stringify(result.summary));
  } finally {
    if (browser.isConnected()) {
      await browser.close();
    }
  }
}

// Run when invoked directly
const isDirectExecution =
  process.argv[1] &&
  new URL(process.argv[1], "file://").href === import.meta.url;

if (isDirectExecution) {
  main();
}
