import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { computeReleaseId, RELEASE_SENTINEL } from './release-v1.mjs';

const directory = path.dirname(fileURLToPath(import.meta.url));
const distDirectory = path.resolve(directory, '..', '..', 'dist');
const FIRST_DISPLAY_ARTIFACT_COUNT = 14;
const CORE_ARTIFACT_INDEX = 1 + FIRST_DISPLAY_ARTIFACT_COUNT;
const INTEGRATION_ARTIFACT_COUNT = 20;
const RELEASE_ARTIFACT_COUNT = CORE_ARTIFACT_INDEX + 1 + INTEGRATION_ARTIFACT_COUNT;
const value = JSON.parse(fs.readFileSync(path.join(distDirectory, 'tsjs-release-v1.json'), 'utf8'));
if (
  typeof value !== 'object' ||
  value === null ||
  Array.isArray(value) ||
  Object.keys(value).join(',') !== 'version,releaseId,artifacts' ||
  value.version !== 1 ||
  !/^[0-9a-f]{64}$/.test(value.releaseId) ||
  !Array.isArray(value.artifacts) ||
  value.artifacts.length !== RELEASE_ARTIFACT_COUNT
) {
  throw new Error('Invalid tsjs-release-v1.json');
}

const normalized = [];
const files = new Set();
for (const [index, artifact] of value.artifacts.entries()) {
  if (
    typeof artifact !== 'object' ||
    artifact === null ||
    Array.isArray(artifact) ||
    Object.keys(artifact).join(',') !== 'id,role,phase,trigger,inputs,outputs,file,bytes,hash' ||
    !/^[a-z0-9][a-z0-9_-]{0,63}$/.test(artifact.id) ||
    !['bootstrap', 'first_display_base', 'first_display_slice', 'core', 'integration'].includes(
      artifact.role
    ) ||
    !Array.isArray(artifact.inputs) ||
    !Array.isArray(artifact.outputs) ||
    !Number.isSafeInteger(artifact.bytes) ||
    artifact.bytes <= 0 ||
    !/^[0-9a-f]{64}$/.test(artifact.hash) ||
    files.has(artifact.file)
  ) {
    throw new Error('Invalid canonical artifact inventory');
  }
  if (
    (index === 0 &&
      (artifact.id !== 'bootstrap' || artifact.role !== 'bootstrap' || artifact.phase !== null)) ||
    (index === 1 &&
      (artifact.id !== 'first_display' ||
        artifact.role !== 'first_display_base' ||
        artifact.phase !== 'first_display')) ||
    (index > 1 &&
      index < CORE_ARTIFACT_INDEX &&
      (artifact.role !== 'first_display_slice' || artifact.phase !== 'first_display')) ||
    (index === CORE_ARTIFACT_INDEX &&
      (artifact.id !== 'core' || artifact.role !== 'core' || artifact.phase !== null)) ||
    (index > CORE_ARTIFACT_INDEX &&
      (artifact.role !== 'integration' || !['takeover', 'deferred'].includes(artifact.phase)))
  ) {
    throw new Error('Invalid canonical artifact role/order');
  }
  files.add(artifact.file);
  const bytes = fs.readFileSync(path.join(distDirectory, artifact.file));
  const source = bytes.toString('utf8');
  if (
    bytes.byteLength !== artifact.bytes ||
    createHash('sha256').update(bytes).digest('hex') !== artifact.hash ||
    source.includes(RELEASE_SENTINEL) ||
    source.split(value.releaseId).length - 1 !== 1
  ) {
    throw new Error(`Artifact release mismatch: ${artifact.file}`);
  }
  normalized.push({
    id: artifact.id,
    role: artifact.role,
    phase: artifact.phase ?? '',
    trigger: artifact.trigger ?? '',
    bytes: Buffer.from(source.replace(value.releaseId, RELEASE_SENTINEL)),
  });
}
if (computeReleaseId(normalized) !== value.releaseId) {
  throw new Error('Release manifest does not match canonical artifact bytes');
}

process.stdout.write(`${value.releaseId}\n`);
