import { createHash } from 'node:crypto';

export const RELEASE_SENTINEL = '__TSJS_RELEASE_ID_SENTINEL_V1__';

export function computeReleaseId(bundles) {
  const hasher = createHash('sha256');
  hasher.update('tsjs-release-v1\0');
  const seen = new Set();
  for (const bundle of bundles) {
    if (!/^[a-z0-9][a-z0-9_-]{0,63}$/.test(bundle.id) || seen.has(bundle.id)) {
      throw new Error('Invalid release bundle id');
    }
    seen.add(bundle.id);
    const bytes = Buffer.isBuffer(bundle.bytes) ? bundle.bytes : Buffer.from(bundle.bytes);
    if (bytes.toString('utf8').split(RELEASE_SENTINEL).length - 1 !== 1) {
      throw new Error(`Expected exactly one release sentinel: ${bundle.id}`);
    }
    hasher.update(`${bundle.id}\0${bytes.byteLength}\0`);
    hasher.update(bytes);
    hasher.update('\0');
  }
  return hasher.digest('hex');
}

export function stampRelease(bytes, releaseId) {
  if (!/^[0-9a-f]{64}$/.test(releaseId)) throw new Error('Invalid release id');
  const source = Buffer.isBuffer(bytes) ? bytes.toString('utf8') : String(bytes);
  if (source.split(RELEASE_SENTINEL).length - 1 !== 1) {
    throw new Error('Expected exactly one release sentinel');
  }
  const stamped = source.replace(RELEASE_SENTINEL, releaseId);
  if (stamped.includes(RELEASE_SENTINEL)) throw new Error('Release sentinel remains');
  return stamped;
}

export function validateStampedRelease(bundles, releaseId, requiredIds) {
  const byId = new Map(bundles.map((bundle) => [bundle.id, bundle.bytes]));
  for (const id of requiredIds) {
    const bytes = byId.get(id);
    if (bytes === undefined) throw new Error(`Missing release bundle: ${id}`);
    const source = Buffer.isBuffer(bytes) ? bytes.toString('utf8') : String(bytes);
    if (source.includes(RELEASE_SENTINEL) || source.split(releaseId).length - 1 !== 1) {
      throw new Error(`Bundle release mismatch: ${id}`);
    }
  }
}
