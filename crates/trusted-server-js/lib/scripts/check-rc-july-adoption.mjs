import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const MANIFEST_FENCE = /```json rcjuly-tsjs-manifest-v1\n([\s\S]*?)\n```/g;
const LEDGER_ID = /\| `(RCJ-[A-Z]+-[0-9]+)`/g;
const QUALITY_ID = 'RCJ-QUAL-01';
const SOURCE_ROOT = 'crates/trusted-server-js/lib/src/';

function sorted(values) {
  return [...values].sort((left, right) => left.localeCompare(right));
}

function gitLines(repositoryRoot, args) {
  const output = execFileSync('git', args, {
    cwd: repositoryRoot,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });

  return output.split('\n').filter(Boolean);
}

function gitObjectExists(repositoryRoot, objectName) {
  try {
    execFileSync('git', ['cat-file', '-e', objectName], {
      cwd: repositoryRoot,
      stdio: 'ignore',
    });
    return true;
  } catch {
    return false;
  }
}

function extractManifest(specSource) {
  const matches = [...specSource.matchAll(MANIFEST_FENCE)];
  if (matches.length !== 1 || typeof matches[0]?.[1] !== 'string') {
    throw new Error(`expected exactly one rcjuly-tsjs-manifest-v1 block, found ${matches.length}`);
  }

  const manifest = JSON.parse(matches[0][1]);
  if (
    manifest === null ||
    typeof manifest !== 'object' ||
    manifest.version !== 1 ||
    typeof manifest.baseline !== 'string' ||
    !Array.isArray(manifest.includeRoots) ||
    !Array.isArray(manifest.mappings)
  ) {
    throw new Error('rc/july adoption manifest has an invalid outer shape');
  }

  return manifest;
}

function mappingMatches(file, mapping) {
  return (
    (Array.isArray(mapping.exact) && mapping.exact.includes(file)) ||
    (typeof mapping.prefix === 'string' && file.startsWith(mapping.prefix)) ||
    (Array.isArray(mapping.prefixes) && mapping.prefixes.some((prefix) => file.startsWith(prefix)))
  );
}

function mappingIdsForFile(file, mappings) {
  const ids = new Set();
  for (const mapping of mappings) {
    if (!mappingMatches(file, mapping)) continue;
    for (const id of mapping.ids ?? []) ids.add(id);
  }
  return ids;
}

export function auditRcJulyAdoption({ repositoryRoot, specPath }) {
  const specSource = fs.readFileSync(specPath, 'utf8');
  const manifest = extractManifest(specSource);
  const files = new Set();

  for (const includeRoot of manifest.includeRoots) {
    for (const file of gitLines(repositoryRoot, [
      'ls-tree',
      '-r',
      '--name-only',
      manifest.baseline,
      '--',
      includeRoot,
    ])) {
      files.add(file);
    }
  }

  for (const mapping of manifest.mappings) {
    for (const file of mapping.exact ?? []) {
      if (gitObjectExists(repositoryRoot, `${manifest.baseline}:${file}`)) files.add(file);
    }
  }

  const orderedFiles = sorted(files);
  const unmappedFiles = orderedFiles.filter(
    (file) => !manifest.mappings.some((mapping) => mappingMatches(file, mapping))
  );
  const qualityOnlySourceFiles = orderedFiles.filter((file) => {
    if (!file.startsWith(SOURCE_ROOT)) return false;
    const ids = mappingIdsForFile(file, manifest.mappings);
    return ![...ids].some((id) => id !== QUALITY_ID);
  });
  const deadMappings = manifest.mappings
    .map((mapping, index) => ({ index, mapping }))
    .filter(({ mapping }) => !orderedFiles.some((file) => mappingMatches(file, mapping)))
    .map(({ index }) => index);

  const manifestIds = new Set(manifest.mappings.flatMap((mapping) => mapping.ids ?? []));
  const ledgerIds = new Set([...specSource.matchAll(LEDGER_ID)].map((match) => match[1]));
  const manifestOnlyIds = sorted([...manifestIds].filter((id) => !ledgerIds.has(id)));
  const ledgerOnlyIds = sorted([...ledgerIds].filter((id) => !manifestIds.has(id)));

  return {
    baseline: manifest.baseline,
    fileCount: orderedFiles.length,
    mappingCount: manifest.mappings.length,
    manifestIdCount: manifestIds.size,
    ledgerIdCount: ledgerIds.size,
    unmappedFiles,
    qualityOnlySourceFiles,
    deadMappings,
    manifestOnlyIds,
    ledgerOnlyIds,
  };
}

export function assertRcJulyAdoption(result) {
  const failures = [];
  if (result.fileCount !== 144) failures.push(`expected 144 files, found ${result.fileCount}`);
  if (result.mappingCount !== 38) {
    failures.push(`expected 38 mappings, found ${result.mappingCount}`);
  }
  if (result.manifestIdCount !== 23 || result.ledgerIdCount !== 23) {
    failures.push(
      `expected 23 manifest/ledger ids, found ${result.manifestIdCount}/${result.ledgerIdCount}`
    );
  }
  for (const key of [
    'unmappedFiles',
    'qualityOnlySourceFiles',
    'deadMappings',
    'manifestOnlyIds',
    'ledgerOnlyIds',
  ]) {
    if (result[key].length > 0) failures.push(`${key}: ${JSON.stringify(result[key])}`);
  }
  if (failures.length > 0) throw new Error(failures.join('\n'));
}

const scriptPath = fileURLToPath(import.meta.url);
if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  const repositoryRoot = path.resolve(path.dirname(scriptPath), '../../../..');
  const specPath = path.join(
    repositoryRoot,
    'docs/superpowers/specs/2026-08-04-aps-render-fix-and-tsjs-resilience-design.md'
  );
  const result = auditRcJulyAdoption({ repositoryRoot, specPath });
  assertRcJulyAdoption(result);
  process.stdout.write(`${JSON.stringify(result)}\n`);
}
