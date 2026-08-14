/** Pure deterministic measurement primitives for frozen TSJS transfer budgets. */

import { createHash } from 'node:crypto';
import {
  brotliCompress,
  brotliCompressSync,
  constants as zlibConstants,
  gzip,
  gzipSync,
} from 'node:zlib';

export const BUNDLE_SET_NAMES = Object.freeze(['minimal', 'reference', 'maximal']);
export const BUNDLE_SIZE_NAMES = Object.freeze(['rawBytes', 'gzipBytes', 'brotliBytes']);
export const BUNDLE_SEPARATOR = Buffer.from(';\n', 'utf8');
export const FIRST_DISPLAY_AGENT_SIZE_CEILING = Object.freeze({
  rawBytes: 90_000,
  gzipBytes: 30_000,
  brotliBytes: 26_000,
});

/** Return whether one exact first-display composition satisfies every transport ceiling. */
export function firstDisplayMaskIsPermitted(measurement) {
  return BUNDLE_SIZE_NAMES.every(
    (sizeName) =>
      Number.isSafeInteger(measurement?.[sizeName]) &&
      measurement[sizeName] > 0 &&
      measurement[sizeName] <= FIRST_DISPLAY_AGENT_SIZE_CEILING[sizeName]
  );
}

const GZIP_OPTIONS = Object.freeze({ level: 9, mtime: 0 });

function brotliOptions(bytes) {
  return {
    params: {
      [zlibConstants.BROTLI_PARAM_MODE]: zlibConstants.BROTLI_MODE_TEXT,
      [zlibConstants.BROTLI_PARAM_QUALITY]: 11,
      [zlibConstants.BROTLI_PARAM_SIZE_HINT]: bytes.byteLength,
    },
  };
}

const REFERENCE_INCLUDE_ORDER = Object.freeze([
  'always',
  'creative_guard',
  'integration:gpt',
  'integration:prebid',
  'integration:datadome',
]);

function fail(message) {
  throw new Error(`[bundle-metrics] ${message}`);
}

function isCatalogModule(module) {
  return (
    module !== null &&
    typeof module === 'object' &&
    typeof module.id === 'string' &&
    ['critical', 'deferred'].includes(module.phase) &&
    (module.trigger === null || module.trigger === 'first_display_or_idle') &&
    typeof module.include === 'string'
  );
}

/** Derive the three semantic transfer sets from catalog phase and inclusion semantics. */
export function deriveSemanticBundleSetIds(modules) {
  if (!Array.isArray(modules) || modules.length === 0 || !modules.every(isCatalogModule)) {
    fail('catalog modules must be a non-empty semantic release catalog');
  }
  const catalogIds = modules.map(({ id }) => id);
  if (new Set(catalogIds).size !== catalogIds.length || catalogIds.includes('core')) {
    fail('catalog modules contain a duplicate or reserved id');
  }
  const critical = modules.filter(({ phase }) => phase === 'critical');
  const reference = REFERENCE_INCLUDE_ORDER.map((include) =>
    critical.filter((module) => module.include === include)
  );
  if (reference.some((matches) => matches.length !== 1)) {
    fail('catalog must define every reference predicate exactly once');
  }
  return {
    minimal: [
      'core',
      ...critical.filter(({ include }) => include === 'always').map(({ id }) => id),
    ],
    reference: ['core', ...reference.map(([module]) => module.id)],
    maximal: ['core', ...catalogIds],
  };
}

function artifactFileById(artifacts) {
  if (!Array.isArray(artifacts) || artifacts.length === 0) {
    fail('release artifacts must be a non-empty array');
  }
  const files = new Map();
  for (const artifact of artifacts) {
    if (
      !artifact ||
      typeof artifact.id !== 'string' ||
      typeof artifact.file !== 'string' ||
      files.has(artifact.id)
    ) {
      fail('release artifacts contain an invalid or duplicate id');
    }
    files.set(artifact.id, artifact.file);
  }
  return files;
}

/** Derive the frozen semantic transfer sets from canonical inventory and catalog data. */
export function deriveInventorySetFiles(artifacts, modules) {
  const files = artifactFileById(artifacts);
  const ids = deriveSemanticBundleSetIds(modules);
  return Object.fromEntries(
    BUNDLE_SET_NAMES.map((setName) => [
      setName,
      ids[setName].map((id) => {
        const file = files.get(id);
        if (!file) fail(`${setName} references missing artifact ${id}`);
        return file;
      }),
    ])
  );
}

/** Measure one byte sequence with the frozen raw, gzip, Brotli, and digest algorithms. */
export function measureBytes(value) {
  if (!(value instanceof Uint8Array)) fail('measurement input must be bytes');
  const bytes = Buffer.from(value.buffer, value.byteOffset, value.byteLength);
  return {
    rawBytes: bytes.byteLength,
    gzipBytes: gzipSync(bytes, GZIP_OPTIONS).byteLength,
    brotliBytes: brotliCompressSync(bytes, brotliOptions(bytes)).byteLength,
    sha256: createHash('sha256').update(bytes).digest('hex'),
  };
}

function compress(compressor, bytes, options) {
  return new Promise((resolve, reject) => {
    compressor(bytes, options, (error, compressed) => {
      if (error) reject(error);
      else resolve(compressed);
    });
  });
}

/** Measure bytes through the same frozen algorithms without blocking mask generation. */
export async function measureBytesAsync(value) {
  if (!(value instanceof Uint8Array)) fail('measurement input must be bytes');
  const bytes = Buffer.from(value.buffer, value.byteOffset, value.byteLength);
  const [gzipBytes, brotliBytes] = await Promise.all([
    compress(gzip, bytes, GZIP_OPTIONS),
    compress(brotliCompress, bytes, brotliOptions(bytes)),
  ]);
  return {
    rawBytes: bytes.byteLength,
    gzipBytes: gzipBytes.byteLength,
    brotliBytes: brotliBytes.byteLength,
    sha256: createHash('sha256').update(bytes).digest('hex'),
  };
}

/** Concatenate and measure a named inventory set without filesystem or process state. */
export function measureBundleSet(files, contents) {
  if (!Array.isArray(files) || files.length === 0 || new Set(files).size !== files.length) {
    fail('bundle set files must be a non-empty unique array');
  }
  if (!(contents instanceof Map)) fail('bundle contents must be a Map');
  const parts = files.flatMap((file, index) => {
    const bytes = contents.get(file);
    if (!(bytes instanceof Uint8Array)) fail(`bundle contents are missing ${file}`);
    return index === files.length - 1 ? [bytes] : [bytes, BUNDLE_SEPARATOR];
  });
  return { files: [...files], ...measureBytes(Buffer.concat(parts)) };
}

function concatenateBundleSet(files, contents) {
  if (!Array.isArray(files) || files.length === 0 || new Set(files).size !== files.length) {
    fail('bundle set files must be a non-empty unique array');
  }
  if (!(contents instanceof Map)) fail('bundle contents must be a Map');
  const parts = files.flatMap((file, index) => {
    const bytes = contents.get(file);
    if (!(bytes instanceof Uint8Array)) fail(`bundle contents are missing ${file}`);
    return index === files.length - 1 ? [bytes] : [bytes, BUNDLE_SEPARATOR];
  });
  return Buffer.concat(parts);
}

/** Enumerate every reachable base mask; APS and Prebid participation require GPT. */
export function enumerateReachableFirstDisplayMasks(catalog) {
  if (!Array.isArray(catalog) || catalog.length !== 13) {
    fail('first-display catalog must contain exactly thirteen rows');
  }
  const ids = new Set();
  const files = new Set();
  for (const [index, row] of catalog.entries()) {
    if (
      !row ||
      typeof row.id !== 'string' ||
      row.maskBit !== index ||
      typeof row.file !== 'string' ||
      ids.has(row.id) ||
      files.has(row.file)
    ) {
      fail(`first-display catalog row ${index} is invalid`);
    }
    ids.add(row.id);
    files.add(row.file);
  }
  if (catalog[0].id !== 'first_display') {
    fail('first-display catalog bit zero must be the base');
  }
  const gpt = catalog.find(({ id }) => id === 'gpt_initial');
  const aps = catalog.find(({ id }) => id === 'aps_initial');
  const prebid = catalog.find(({ id }) => id === 'prebid_initial');
  if (!gpt) fail('first-display catalog must contain gpt_initial');
  if (!aps || !prebid) fail('first-display catalog must contain APS and Prebid participation');
  const required = 1 << catalog[0].maskBit;
  const maximumMask = 1 << catalog.length;
  const result = [];
  for (let mask = 0; mask < maximumMask; mask += 1) {
    if ((mask & required) !== required) continue;
    const hasGpt = (mask & (1 << gpt.maskBit)) !== 0;
    if (!hasGpt && ((mask & (1 << aps.maskBit)) !== 0 || (mask & (1 << prebid.maskBit)) !== 0)) {
      continue;
    }
    const selected = catalog.filter(({ maskBit }) => (mask & (1 << maskBit)) !== 0);
    result.push({
      mask: mask.toString(16).padStart(4, '0'),
      ids: selected.map(({ id }) => id),
      files: selected.map(({ file }) => file),
    });
  }
  return result;
}

/** Measure and hash every reachable first-display mask with bounded async compression work. */
export async function measureReachableFirstDisplayMasks(catalog, contents, concurrency = 8) {
  if (!Number.isSafeInteger(concurrency) || concurrency < 1 || concurrency > 64) {
    fail('first-display mask measurement concurrency must be between 1 and 64');
  }
  const masks = enumerateReachableFirstDisplayMasks(catalog);
  const measured = new Array(masks.length);
  let next = 0;
  const worker = async () => {
    while (next < masks.length) {
      const index = next;
      next += 1;
      const mask = masks[index];
      measured[index] = {
        ...mask,
        ...(await measureBytesAsync(concatenateBundleSet(mask.files, contents))),
      };
      measured[index].permitted = firstDisplayMaskIsPermitted(measured[index]);
    }
  };
  await Promise.all(Array.from({ length: Math.min(concurrency, masks.length) }, worker));
  return measured;
}
