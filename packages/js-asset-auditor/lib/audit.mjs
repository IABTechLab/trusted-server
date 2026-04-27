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
//   --domain <domain>   Override publisher domain used for slug generation
//   --settle <ms>       Settle window after page load (default: 6000)
//   --first-party <h>   Additional first-party hosts (comma-separated)
//   --no-filter         Bypass heuristic filtering
//   --headless          Run browser headlessly
//   --output <path>     Output file path (default: js-assets.toml)
//   --config [path]     Generate trusted-server.toml (default path: trusted-server.generated.toml)
//   --force             Overwrite config file when used with --config
//
// Prerequisites:
//   cd packages/js-asset-auditor && npm install && npx playwright install chromium

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { processAssets } from "./process.mjs";

const DEFAULT_GENERATED_CONFIG_PATH = "trusted-server.generated.toml";

const USAGE =
  "Usage: audit-js-assets <url> [--diff] [--domain <domain>] [--settle <ms>] [--first-party <hosts>] [--no-filter] [--headless] [--output <path>] [--config [path]] [--force]";

// ---------------------------------------------------------------------------
// Config reading
// ---------------------------------------------------------------------------

export function readPublisherDomain(repoRoot, configPath = "trusted-server.toml") {
  const resolvedPath = resolve(repoRoot, configPath);
  const content = readFileSync(resolvedPath, "utf-8");
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
      const match = line.match(/^domain\s*=\s*"([^"]+)"/);
      if (match) return match[1];
    }
  }

  throw new Error(
    `Could not find [publisher].domain in ${configPath}`,
  );
}

function inferDomainFromTarget(target) {
  try {
    const host = new URL(target).hostname;
    return host.startsWith("www.") ? host.slice(4) : host;
  } catch {
    return target;
  }
}

function exitWithUsage(message) {
  console.error(message);
  console.error(USAGE);
  process.exit(1);
}

function requireFlagValue(argv, index, flag) {
  const value = argv[index + 1];
  if (!value || value.startsWith("--")) {
    exitWithUsage(`${flag} requires a value`);
  }
  return value;
}

export function fail(message) {
  console.error(message);
  process.exit(1);
}

export function resolvePublisherDomain(args, repoRoot) {
  if (args.domain) {
    console.error(
      `Using publisher domain from --domain: ${args.domain}`,
    );
    return args.domain;
  }

  try {
    const domain = readPublisherDomain(repoRoot);
    console.error(
      `Using publisher domain from trusted-server.toml: ${domain}`,
    );
    return domain;
  } catch (error) {
    if (error?.code === "ENOENT") {
      const domain = inferDomainFromTarget(args.url);
      console.error(
        `No trusted-server.toml found, inferring publisher domain from target URL: ${domain}`,
      );
      return domain;
    }

    fail(
      `Failed to read publisher domain from trusted-server.toml: ${error.message}`,
    );
  }
}

export function ensureConfigPathWritable(configPath, force) {
  if (existsSync(configPath) && !force) {
    fail(`${configPath} already exists. Use --force to overwrite.`);
  }
}

// ---------------------------------------------------------------------------
// CLI argument parsing
// ---------------------------------------------------------------------------

export function parseArgs(argv) {
  const args = {
    url: null,
    domain: null,
    diff: false,
    settle: 6000,
    firstParty: [],
    noFilter: false,
    headless: false,
    output: "js-assets.toml",
    config: null,
    force: false,
  };

  for (let i = 2; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--domain") {
      args.domain = requireFlagValue(argv, i, "--domain");
      i += 1;
    } else if (arg === "--diff") {
      args.diff = true;
    } else if (arg === "--settle") {
      const settleValue = requireFlagValue(argv, i, "--settle");
      const parsedSettle = Number.parseInt(settleValue, 10);
      if (!Number.isFinite(parsedSettle) || parsedSettle < 0) {
        exitWithUsage("--settle requires a non-negative integer value");
      }
      args.settle = parsedSettle;
      i += 1;
    } else if (arg === "--first-party") {
      const firstPartyValue = requireFlagValue(argv, i, "--first-party");
      args.firstParty = firstPartyValue.split(",").filter(Boolean);
      i += 1;
    } else if (arg === "--no-filter") {
      args.noFilter = true;
    } else if (arg === "--headless") {
      args.headless = true;
    } else if (arg === "--output") {
      args.output = requireFlagValue(argv, i, "--output");
      i += 1;
    } else if (arg === "--config") {
      const next = argv[i + 1];
      if (next && !next.startsWith("--")) {
        args.config = next;
        i += 1;
      } else {
        args.config = DEFAULT_GENERATED_CONFIG_PATH;
      }
    } else if (arg === "--force") {
      args.force = true;
    } else if (!arg.startsWith("--") && !args.url) {
      args.url = arg.startsWith("http") ? arg : `https://${arg}`;
    } else {
      exitWithUsage(`Unknown argument: ${arg}`);
    }
  }

  if (!args.url) {
    exitWithUsage("Missing required <url> argument");
  }

  return args;
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

export async function main() {
  const args = parseArgs(process.argv);
  const repoRoot = process.cwd();

  const domain = resolvePublisherDomain(args, repoRoot);

  let chromium;
  try {
    ({ chromium } = await import("playwright"));
  } catch {
    console.error(
      "Playwright not installed. Run:\n  cd packages/js-asset-auditor && npm install",
    );
    process.exit(1);
  }

  console.error("Launching browser...");
  let browser;
  try {
    browser = await chromium.launch({ headless: args.headless });
  } catch (error) {
    if (error.message.includes("Executable doesn't exist")) {
      console.error(
        "Chromium not installed. Run:\n  cd packages/js-asset-auditor && npx playwright install chromium",
      );
      process.exit(1);
    }
    throw error;
  }

  try {
    const context = await browser.newContext();
    const page = await context.newPage();

    const scriptUrls = [];
    page.on("response", (response) => {
      const request = response.request();
      if (request.resourceType() === "script") {
        scriptUrls.push(request.url());
      }
    });

    console.error(`Navigating to ${args.url}...`);
    await page.goto(args.url, { waitUntil: "load", timeout: 30000 });

    console.error(`Waiting ${args.settle}ms for page to settle...`);
    await page.waitForTimeout(args.settle);

    const headScriptUrls = await page.evaluate(() =>
      Array.from(document.head.querySelectorAll("script[src]")).map(
        (script) => script.src,
      ),
    );

    const runtimeSignals = await page.evaluate(() => {
      const prebidBidders = new Set();
      for (const adUnit of window.pbjs?.adUnits ?? []) {
        for (const bid of adUnit.bids ?? []) {
          if (typeof bid.bidder === "string" && bid.bidder.length > 0) {
            prebidBidders.add(bid.bidder);
          }
        }
      }

      return {
        prebidBidders: Array.from(prebidBidders).sort(),
      };
    });

    console.error(
      `Found ${scriptUrls.length} network scripts, ${headScriptUrls.length} head scripts`,
    );
    if (runtimeSignals.prebidBidders.length > 0) {
      console.error(
        `Detected Prebid bidders: ${runtimeSignals.prebidBidders.join(", ")}`,
      );
    }

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

    if (args.config) {
      const { detectIntegrations, generateConfig } = await import("./detect.mjs");
      const detection = detectIntegrations(scriptUrls, runtimeSignals);

      if (detection.integrations.length > 0) {
        ensureConfigPathWritable(args.config, args.force);
        const configToml = generateConfig(domain, args.url, detection);
        writeFileSync(args.config, configToml);
        console.error(
          `Wrote ${args.config} (${detection.integrations.length} integrations detected)`,
        );
      } else {
        console.error("No integrations detected — skipping config generation");
      }

      result.summary.integrations = detection.integrations.map((integration) => ({
        id: integration.id,
        label: integration.label,
        category: integration.category,
        extracted: integration.extracted,
        todos: integration.todos,
      }));
    }

    console.log(JSON.stringify(result.summary));
  } finally {
    if (browser?.isConnected()) {
      await browser.close();
    }
  }
}

const isDirectExecution =
  process.argv[1] &&
  new URL(process.argv[1], "file://").href === import.meta.url;

if (isDirectExecution) {
  main();
}
