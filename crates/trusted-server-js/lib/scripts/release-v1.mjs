import { createHash } from 'node:crypto';

export const RELEASE_SENTINEL = '__TSJS_RELEASE_ID_SENTINEL_V1__';

const RELEASE_PREFIX = Buffer.from('tsjs-release-v1\0', 'ascii');

function u64(value) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error('Invalid release frame length');
  const bytes = Buffer.alloc(8);
  bytes.writeBigUInt64BE(BigInt(value));
  return bytes;
}

function framed(value) {
  const bytes = Buffer.isBuffer(value) ? value : Buffer.from(value, 'utf8');
  return Buffer.concat([u64(bytes.byteLength), bytes]);
}

function validateArtifact(artifact, seen) {
  if (!/^[a-z0-9][a-z0-9_-]{0,63}$/.test(artifact.id) || seen.has(artifact.id)) {
    throw new Error('Invalid release bundle id');
  }
  for (const field of ['role', 'phase', 'trigger']) {
    if (typeof artifact[field] !== 'string') {
      throw new Error(`Invalid release artifact ${field}: ${artifact.id}`);
    }
  }
  seen.add(artifact.id);
}

export function computeReleaseId(artifacts) {
  const hasher = createHash('sha256');
  hasher.update(RELEASE_PREFIX);
  hasher.update(u64(artifacts.length));
  const seen = new Set();
  for (const artifact of artifacts) {
    validateArtifact(artifact, seen);
    const bytes = Buffer.isBuffer(artifact.bytes)
      ? artifact.bytes
      : Buffer.from(artifact.bytes, 'utf8');
    if (bytes.toString('utf8').split(RELEASE_SENTINEL).length - 1 !== 1) {
      throw new Error(`Expected exactly one release sentinel: ${artifact.id}`);
    }
    hasher.update(framed(artifact.id));
    hasher.update(framed(artifact.role));
    hasher.update(framed(artifact.phase));
    hasher.update(framed(artifact.trigger));
    hasher.update(framed(bytes));
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
