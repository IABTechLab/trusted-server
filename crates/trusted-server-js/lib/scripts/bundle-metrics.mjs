/** Pure deterministic measurement primitives for frozen TSJS transfer budgets. */

import { createHash } from 'node:crypto';
import { brotliCompressSync, constants as zlibConstants, gzipSync } from 'node:zlib';

export const BUNDLE_SET_NAMES = Object.freeze(['minimal', 'reference', 'maximal']);
export const BUNDLE_SIZE_NAMES = Object.freeze(['rawBytes', 'gzipBytes', 'brotliBytes']);
export const BUNDLE_SEPARATOR = Buffer.from(';\n', 'utf8');

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
    gzipBytes: gzipSync(bytes, { level: 9, mtime: 0 }).byteLength,
    brotliBytes: brotliCompressSync(bytes, {
      params: {
        [zlibConstants.BROTLI_PARAM_MODE]: zlibConstants.BROTLI_MODE_TEXT,
        [zlibConstants.BROTLI_PARAM_QUALITY]: 11,
        [zlibConstants.BROTLI_PARAM_SIZE_HINT]: bytes.byteLength,
      },
    }).byteLength,
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
