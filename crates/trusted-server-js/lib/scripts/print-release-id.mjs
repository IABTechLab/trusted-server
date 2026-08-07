import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { computeReleaseId, RELEASE_SENTINEL } from './release-v1.mjs';

const directory = path.dirname(fileURLToPath(import.meta.url));
const distDirectory = path.resolve(directory, '..', '..', 'dist');
const manifestPath = path.join(distDirectory, 'tsjs-release-v1.json');
const value = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));

if (
  typeof value !== 'object' ||
  value === null ||
  Array.isArray(value) ||
  Object.keys(value).join(',') !== 'version,releaseId,bundles' ||
  value.version !== 1 ||
  !/^[0-9a-f]{64}$/.test(value.releaseId) ||
  !Array.isArray(value.bundles) ||
  value.bundles.length === 0
) {
  throw new Error('Invalid tsjs-release-v1.json');
}

let previous = '';
const normalizedBundles = [];
for (let index = 0; index < value.bundles.length; index += 1) {
  const bundle = value.bundles[index];
  if (
    typeof bundle !== 'object' ||
    bundle === null ||
    Array.isArray(bundle) ||
    Object.keys(bundle).join(',') !== 'id,file' ||
    !/^[a-z0-9][a-z0-9_-]{0,63}$/.test(bundle.id) ||
    bundle.file !== `tsjs-${bundle.id}.js` ||
    (index === 0 ? bundle.id !== 'core' : bundle.id <= previous)
  ) {
    throw new Error('Invalid canonical bundle inventory');
  }
  const source = fs.readFileSync(path.join(distDirectory, bundle.file), 'utf8');
  if (
    source.includes('__TSJS_RELEASE_ID_SENTINEL_V1__') ||
    source.split(value.releaseId).length - 1 !== 1
  ) {
    throw new Error(`Bundle release id mismatch: ${bundle.file}`);
  }
  normalizedBundles.push({
    id: bundle.id,
    bytes: Buffer.from(source.replace(value.releaseId, RELEASE_SENTINEL)),
  });
  previous = bundle.id;
}
const discovered = fs
  .readdirSync(distDirectory)
  .filter((file) => file.startsWith('tsjs-') && file.endsWith('.js'))
  .sort((left, right) => {
    if (left === 'tsjs-core.js') return -1;
    if (right === 'tsjs-core.js') return 1;
    return left < right ? -1 : left > right ? 1 : 0;
  });
if (
  discovered.join(',') !== value.bundles.map(({ file }) => file).join(',') ||
  computeReleaseId(normalizedBundles) !== value.releaseId
) {
  throw new Error('Release manifest does not match canonical bundle bytes');
}
const fallback = fs.readFileSync(path.join(distDirectory, 'gpt-bootstrap-fallback.js'), 'utf8');
if (
  fallback.includes('__TSJS_RELEASE_ID_SENTINEL_V1__') ||
  fallback.split(value.releaseId).length - 1 !== 1
) {
  throw new Error('Generated fallback release id mismatch');
}

process.stdout.write(`${value.releaseId}\n`);
