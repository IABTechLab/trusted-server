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

import { generateSlug } from "../packages/js-asset-auditor/lib/process.mjs";

const [publisherDomain, originUrl] = process.argv.slice(2);

if (!publisherDomain || !originUrl) {
  console.error(
    "Usage: node scripts/js-asset-slug.mjs <publisher_domain> <origin_url>",
  );
  process.exit(1);
}

console.log(generateSlug(publisherDomain, originUrl));
