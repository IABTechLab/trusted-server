// JS Asset Auditor — Processing Library
//
// Pure processing functions for URL normalization, filtering, wildcard
// detection, slug generation, and TOML formatting. No external dependencies.
//
// This file is the canonical slug-generation implementation. Standalone CLIs
// import from here to avoid drift.

import { createHash } from "node:crypto";
import { posix } from "node:path";
import { readFileSync } from "node:fs";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const BASE62_CHARSET =
  "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

export const HEURISTIC_FILTERS = {
  "Framework CDNs": ["cdnjs.cloudflare.com", "ajax.googleapis.com", "cdn.jsdelivr.net", "unpkg.com"],
  "Error tracking": ["sentry.io", "bugsnag.com", "rollbar.com"],
  "Font services": ["fonts.googleapis.com", "fonts.gstatic.com"],
  "Social embeds": ["platform.twitter.com", "platform.x.com", "connect.facebook.net"],
  "Google ad rendering": [
    "pagead2.googlesyndication.com",
    "tpc.googlesyndication.com",
    "s0.2mdn.net",
    "googleads.g.doubleclick.net",
    "www.googleadservices.com",
  ],
  "Ad fraud detection": ["adtrafficquality.google"],
  "Ad verification": ["adsafeprotected.com", "moatads.com", "doubleverify.com"],
  reCAPTCHA: [
    "recaptcha.net",
    { host: "www.google.com", pathPrefix: "/recaptcha/" },
    { host: "www.gstatic.com", pathPrefix: "/recaptcha/" },
  ],
};

const SEMVER_RE = /^\d+\.\d+[\.\d\w-]*$/;
const HEX_HASH_RE = /^[a-f0-9]{8,}$/;
const MIXED_HASH_RE = /^[A-Za-z0-9]{8,}$/;

// ---------------------------------------------------------------------------
// Slug generation
// ---------------------------------------------------------------------------

function bufferToBase62(buffer) {
  let num = 0n;
  for (const byte of buffer) {
    num = (num << 8n) | BigInt(byte);
  }
  if (num === 0n) return "0";
  const chars = [];
  while (num > 0n) {
    chars.push(BASE62_CHARSET[Number(num % 62n)]);
    num = num / 62n;
  }
  return chars.reverse().join("");
}

export function extractAssetStem(originUrl) {
  let pathname;
  try {
    pathname = new URL(originUrl).pathname;
  } catch {
    pathname = originUrl;
  }
  if (pathname.endsWith("/")) pathname = pathname.slice(0, -1);
  const basename = posix.basename(pathname);
  if (!basename || basename === "/") {
    const segments = pathname.split("/").filter(Boolean);
    const last = segments.at(-1) || "unknown";
    const dot = last.lastIndexOf(".");
    return dot > 0 ? last.slice(0, dot) : last;
  }
  const dot = basename.lastIndexOf(".");
  return dot > 0 ? basename.slice(0, dot) : basename;
}

export function generateSlug(publisherDomain, originUrl) {
  const input = `${publisherDomain}|${originUrl}`;
  const digest = createHash("sha256").update(input).digest();
  const base62 = bufferToBase62(digest);
  const publisherPrefix = base62.slice(0, 8);
  const assetStem = extractAssetStem(originUrl);
  return `${publisherPrefix}:${assetStem}`;
}

// ---------------------------------------------------------------------------
// URL processing
// ---------------------------------------------------------------------------

export function normalizeUrl(raw) {
  let url = raw;
  if (url.startsWith("//")) url = "https:" + url;
  const hashIdx = url.indexOf("#");
  if (hashIdx !== -1) url = url.slice(0, hashIdx);
  const qIdx = url.indexOf("?");
  if (qIdx !== -1) url = url.slice(0, qIdx);
  if (url.endsWith("/")) url = url.slice(0, -1);
  return url;
}

function stripWww(host) {
  return host.startsWith("www.") ? host.slice(4) : host;
}

export function isFirstParty(hostname, publisherDomain, targetHost, extraHosts) {
  const stripped = stripWww(hostname);
  if (stripped === stripWww(publisherDomain)) return true;
  if (stripped === stripWww(targetHost)) return true;
  for (const h of extraHosts) {
    if (stripped === stripWww(h)) return true;
  }
  return false;
}

function dotBoundaryMatch(hostname, filterEntry) {
  return hostname === filterEntry || hostname.endsWith("." + filterEntry);
}

export function matchesHeuristicFilter(hostname, pathname) {
  for (const [category, entries] of Object.entries(HEURISTIC_FILTERS)) {
    for (const entry of entries) {
      if (typeof entry === "string") {
        if (dotBoundaryMatch(hostname, entry)) {
          return { category, entry };
        }
      } else {
        if (
          dotBoundaryMatch(hostname, entry.host) &&
          pathname.startsWith(entry.pathPrefix)
        ) {
          return { category, entry: `${entry.host}${entry.pathPrefix}*` };
        }
      }
    }
  }
  return null;
}

// ---------------------------------------------------------------------------
// Wildcard detection
// ---------------------------------------------------------------------------

export function applyWildcards(url) {
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    return { wildcarded: url, original: null, hasWildcard: false };
  }
  const segments = parsed.pathname.split("/");
  let hasWildcard = false;
  const newSegments = segments.map((seg) => {
    if (!seg) return seg;
    if (SEMVER_RE.test(seg)) {
      hasWildcard = true;
      return "*";
    }
    if (HEX_HASH_RE.test(seg)) {
      hasWildcard = true;
      return "*";
    }
    if (
      MIXED_HASH_RE.test(seg) &&
      /\d/.test(seg) &&
      /[a-zA-Z]/.test(seg)
    ) {
      hasWildcard = true;
      return "*";
    }
    return seg;
  });
  const wildcarded = parsed.origin + newSegments.join("/");
  return { wildcarded, original: hasWildcard ? url : null, hasWildcard };
}

// ---------------------------------------------------------------------------
// TOML formatting
// ---------------------------------------------------------------------------

export function formatTomlEntry(asset, commented = false) {
  const pfx = commented ? "# " : "";
  let block = "";
  if (asset.hasWildcard && asset.originalUrl) {
    block += `${pfx}# ${asset.originalUrl} (wildcard detected)\n`;
  }
  block += `${pfx}slug = "${asset.slug}"\n`;
  block += `${pfx}path = "${asset.path}"\n`;
  block += `${pfx}origin_url = "${asset.originUrl}"\n`;
  block += `${pfx}inject_in_head = ${asset.injectInHead}\n`;
  return block;
}

export function shortenUrl(url) {
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    return url;
  }
  const parts = parsed.pathname.split("/").filter(Boolean);
  const filename = parts.at(-1) || parsed.pathname;
  return `${parsed.hostname}/.../` + filename;
}

// ---------------------------------------------------------------------------
// Diff mode: parse existing TOML
// ---------------------------------------------------------------------------

export function parseExistingToml(content) {
  const entries = [];
  const blocks = content.split("[[js_assets]]");
  for (let i = 1; i < blocks.length; i++) {
    const block = blocks[i];
    const originMatch = block.match(/^origin_url\s*=\s*"([^"]+)"/m);
    const slugMatch = block.match(/^slug\s*=\s*"([^"]+)"/m);
    if (originMatch) {
      entries.push({
        originUrl: originMatch[1],
        slug: slugMatch ? slugMatch[1] : "",
      });
    }
  }
  return entries;
}

// ---------------------------------------------------------------------------
// Core processing pipeline
// ---------------------------------------------------------------------------

export function processAssets(input, args) {
  const { networkUrls: rawNetworkUrls, headUrls: rawHeadUrls } = input;

  let targetHost = "";
  try {
    targetHost = new URL(args.target).hostname;
  } catch {
    targetHost = args.target;
  }

  const normalizedNetwork = [...new Set(rawNetworkUrls.map(normalizeUrl))];
  const normalizedHead = new Set(rawHeadUrls.map(normalizeUrl));

  const firstPartyFiltered = [];
  const thirdPartyUrls = [];

  for (const url of normalizedNetwork) {
    let hostname;
    try {
      hostname = new URL(url).hostname;
    } catch {
      continue;
    }
    if (isFirstParty(hostname, args.domain, targetHost, args.firstParty || [])) {
      firstPartyFiltered.push({ url, host: hostname });
    } else {
      thirdPartyUrls.push(url);
    }
  }

  const heuristicFiltered = [];
  const survivingUrls = [];

  for (const url of thirdPartyUrls) {
    let hostname, pathname;
    try {
      const parsed = new URL(url);
      hostname = parsed.hostname;
      pathname = parsed.pathname;
    } catch {
      survivingUrls.push(url);
      continue;
    }

    if (args.noFilter) {
      survivingUrls.push(url);
      continue;
    }

    const match = matchesHeuristicFilter(hostname, pathname);
    if (match) {
      heuristicFiltered.push({ url, host: hostname, ...match });
    } else {
      survivingUrls.push(url);
    }
  }

  const filterCounts = {};
  for (const f of heuristicFiltered) {
    filterCounts[f.host] = (filterCounts[f.host] || 0) + 1;
  }

  const assets = [];
  const seenOrigins = new Set();

  for (const url of survivingUrls) {
    const { wildcarded, original, hasWildcard } = applyWildcards(url);

    if (seenOrigins.has(wildcarded)) continue;
    seenOrigins.add(wildcarded);

    const slug = generateSlug(args.domain, wildcarded);
    const prefix = slug.split(":")[0];
    const injectInHead = normalizedHead.has(url);

    let path;
    if (hasWildcard) {
      path = `/js-assets/${prefix}/*`;
    } else {
      const stem = extractAssetStem(wildcarded);
      path = `/js-assets/${prefix}/${stem}.js`;
    }

    let hostname;
    try {
      hostname = new URL(url).hostname;
    } catch {
      hostname = "unknown";
    }

    assets.push({
      slug,
      prefix,
      path,
      originUrl: wildcarded,
      originalUrl: original,
      injectInHead,
      hasWildcard,
      host: hostname,
      shortUrl: shortenUrl(wildcarded),
    });
  }

  const today = new Date().toISOString().slice(0, 10);

  if (args.diff) {
    let existingContent;
    try {
      existingContent = readFileSync(args.output, "utf-8");
    } catch {
      return { error: `Cannot read ${args.output} for diff mode` };
    }

    const existingEntries = parseExistingToml(existingContent);
    const existingOrigins = new Set(existingEntries.map((e) => e.originUrl));
    const sweepOrigins = new Set(assets.map((a) => a.originUrl));

    const confirmed = existingEntries.filter((e) => sweepOrigins.has(e.originUrl));
    const missing = existingEntries.filter((e) => !sweepOrigins.has(e.originUrl));
    const newAssets = assets.filter((a) => !existingOrigins.has(a.originUrl));

    let appendBlock = "";
    if (newAssets.length > 0) {
      appendBlock = `\n# --- NEW (detected by /audit-js-assets --diff on ${today}, uncomment to activate) ---\n`;
      for (const a of newAssets) {
        appendBlock += `\n# [[js_assets]]\n`;
        appendBlock += formatTomlEntry(a, true);
      }
    }

    return {
      toml: existingContent + appendBlock,
      summary: {
        mode: "diff",
        publisherDomain: args.domain,
        targetUrl: args.target,
        confirmed: confirmed.map((e) => ({ slug: e.slug, originUrl: e.originUrl })),
        new: newAssets.map((a) => ({ slug: a.slug, prefix: a.prefix, shortUrl: a.shortUrl, originUrl: a.originUrl })),
        missing: missing.map((e) => ({ slug: e.slug, originUrl: e.originUrl })),
        outputFile: args.output,
      },
    };
  }

  let toml = `# Generated by /audit-js-assets on ${today}\n`;
  toml += `# Publisher: ${args.domain}\n`;
  toml += `# Source URL: ${args.target}\n`;

  for (const a of assets) {
    toml += `\n[[js_assets]]\n`;
    toml += formatTomlEntry(a);
  }

  const filterSummary = Object.entries(filterCounts).map(([host, count]) => ({ host, count }));

  return {
    toml,
    summary: {
      mode: "init",
      publisherDomain: args.domain,
      targetUrl: args.target,
      totalDetected: thirdPartyUrls.length,
      firstPartyFiltered: firstPartyFiltered.length,
      firstPartyHost: targetHost,
      heuristicFiltered: filterSummary,
      heuristicFilteredTotal: heuristicFiltered.length,
      surfaced: assets.length,
      assets: assets.map((a) => ({
        prefix: a.prefix,
        injectInHead: a.injectInHead,
        shortUrl: a.shortUrl,
        wildcard: a.hasWildcard,
      })),
      outputFile: args.output,
    },
  };
}
