#!/usr/bin/env node

// JS Asset Auditor CLI
//
// Standalone Playwright-based tool that sweeps a publisher page for third-party
// JS assets and generates js-assets.toml entries. Fully deterministic — no LLM
// involvement.
//
// Usage:
//   node tools/js-asset-auditor/audit.mjs https://www.publisher.com [options]
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
//   cd tools/js-asset-auditor && npm install && npx playwright install chromium

import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { processAssets } from "../../scripts/audit-js-assets.mjs";

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
    diff: false,
    settle: 6000,
    firstParty: [],
    noFilter: false,
    headed: false,
    output: "js-assets.toml",
  };

  for (let i = 2; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--diff") {
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
      // Positional argument: the URL
      args.url = arg.startsWith("http") ? arg : `https://${arg}`;
    } else {
      console.error(`Unknown argument: ${arg}`);
      process.exit(1);
    }
  }

  if (!args.url) {
    console.error(
      "Usage: node tools/js-asset-auditor/audit.mjs <url> [--diff] [--settle <ms>] [--first-party <hosts>] [--no-filter] [--headed] [--output <path>]",
    );
    process.exit(1);
  }

  return args;
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function main() {
  const args = parseArgs(process.argv);
  const repoRoot = process.cwd();

  // Read publisher domain from config
  let domain;
  try {
    domain = readPublisherDomain(repoRoot);
  } catch (err) {
    console.error(err.message);
    process.exit(1);
  }

  // Import Playwright
  let chromium;
  try {
    ({ chromium } = await import("playwright"));
  } catch {
    console.error(
      "Playwright not installed. Run:\n  cd tools/js-asset-auditor && npm install",
    );
    process.exit(1);
  }

  // Launch browser
  console.error(`Launching browser...`);
  let browser;
  try {
    browser = await chromium.launch({ headless: !args.headed });
  } catch (err) {
    if (err.message.includes("Executable doesn't exist")) {
      console.error(
        "Chromium not installed. Run:\n  cd tools/js-asset-auditor && npx playwright install chromium",
      );
      process.exit(1);
    }
    throw err;
  }

  try {
    const context = await browser.newContext();
    const page = await context.newPage();

    // Collect script network requests
    const scriptUrls = [];
    page.on("response", (response) => {
      const req = response.request();
      if (req.resourceType() === "script") {
        scriptUrls.push(req.url());
      }
    });

    // Navigate
    console.error(`Navigating to ${args.url}...`);
    await page.goto(args.url, { waitUntil: "load", timeout: 30000 });

    // Settle
    console.error(`Waiting ${args.settle}ms for page to settle...`);
    await page.waitForTimeout(args.settle);

    // Collect head scripts from DOM
    const headScriptUrls = await page.evaluate(() =>
      Array.from(
        document.head.querySelectorAll("script[src]"),
      ).map((s) => s.src),
    );

    console.error(
      `Found ${scriptUrls.length} network scripts, ${headScriptUrls.length} head scripts`,
    );

    await browser.close();

    // Process
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

    // Write output
    writeFileSync(args.output, result.toml);
    const count =
      result.summary.mode === "init"
        ? result.summary.surfaced
        : result.summary.new.length;
    console.error(`Wrote ${args.output} (${count} entries)`);

    // Print JSON summary to stdout
    console.log(JSON.stringify(result.summary));
  } finally {
    if (browser.isConnected()) {
      await browser.close();
    }
  }
}

main();
