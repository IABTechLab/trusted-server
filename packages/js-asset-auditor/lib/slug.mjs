#!/usr/bin/env node

// JS Asset Slug Generator
//
// Shared utility for generating deterministic slugs for js-assets.toml entries.
// Must produce identical output to the Rust proxy's KV key derivation.
//
// Algorithm:
//   publisher_prefix = first_8_chars(base62(sha256(domain + "|" + url)))
//   asset_stem       = filename_without_extension(url)
//   slug             = "{publisher_prefix}:{asset_stem}"
//
// Usage:
//   node packages/js-asset-auditor/lib/slug.mjs <publisher_domain> <origin_url>

import { generateSlug } from "./process.mjs";

const [publisherDomain, originUrl] = process.argv.slice(2);

if (!publisherDomain || !originUrl) {
  console.error(
    "Usage: node packages/js-asset-auditor/lib/slug.mjs <publisher_domain> <origin_url>",
  );
  process.exit(1);
}

console.log(generateSlug(publisherDomain, originUrl));
