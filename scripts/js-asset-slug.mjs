#!/usr/bin/env node

// JS Asset Slug Generator
//
// Shared utility for generating deterministic slugs for js-assets.toml entries.
// Used by the /audit-js-assets command and must produce identical output to the
// Rust proxy's KV key derivation.
//
// Algorithm:
//   publisher_prefix = first_8_chars(base62(sha256(domain + "|" + url)))
//   asset_stem       = filename_without_extension(url)
//   slug             = "{publisher_prefix}:{asset_stem}"
//
// base62 charset: 0-9A-Za-z (digits first, then uppercase, then lowercase)
//
// Usage:
//   node scripts/js-asset-slug.mjs <publisher_domain> <origin_url>
//   node scripts/js-asset-slug.mjs test-publisher.com https://vendor.io/sdk/loader.js
//   # Output: <8-char-prefix>:loader

import { createHash } from "node:crypto";
import { posix } from "node:path";

const BASE62_CHARSET =
  "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

function bufferToBase62(buffer) {
  // Treat the buffer as a big-endian unsigned integer and convert to base62.
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

function extractAssetStem(originUrl) {
  let pathname;
  try {
    pathname = new URL(originUrl).pathname;
  } catch {
    pathname = originUrl;
  }

  // Remove trailing slash
  if (pathname.endsWith("/")) {
    pathname = pathname.slice(0, -1);
  }

  const basename = posix.basename(pathname);
  if (!basename || basename === "/") {
    // Fallback: use last non-empty path segment
    const segments = pathname.split("/").filter(Boolean);
    const last = segments.at(-1) || "unknown";
    const dot = last.lastIndexOf(".");
    return dot > 0 ? last.slice(0, dot) : last;
  }

  const dot = basename.lastIndexOf(".");
  return dot > 0 ? basename.slice(0, dot) : basename;
}

function generateSlug(publisherDomain, originUrl) {
  const input = `${publisherDomain}|${originUrl}`;
  const digest = createHash("sha256").update(input).digest();
  const base62 = bufferToBase62(digest);
  const publisherPrefix = base62.slice(0, 8);
  const assetStem = extractAssetStem(originUrl);
  return `${publisherPrefix}:${assetStem}`;
}

const [publisherDomain, originUrl] = process.argv.slice(2);

if (!publisherDomain || !originUrl) {
  console.error(
    "Usage: node scripts/js-asset-slug.mjs <publisher_domain> <origin_url>",
  );
  process.exit(1);
}

console.log(generateSlug(publisherDomain, originUrl));
