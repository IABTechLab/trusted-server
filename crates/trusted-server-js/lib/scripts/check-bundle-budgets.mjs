#!/usr/bin/env node

import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

import {
  BUNDLE_SET_NAMES,
  BUNDLE_SIZE_NAMES,
  deriveInventorySetFiles,
  deriveSemanticBundleSetIds,
  measureBundleSet,
  measureBytes,
} from './bundle-metrics.mjs';
import { computeReleaseId, RELEASE_SENTINEL } from './release-v1.mjs';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const libDir = path.resolve(scriptDir, '..');
const defaultBaselinePath = path.join(
  libDir,
  'test',
  'fixtures',
  'performance',
  'aps-tsjs-prechange.json'
);
const metricsPath = path.resolve(libDir, '..', 'dist', 'tsjs-build-metrics-v1.json');
const catalogPath = path.resolve(libDir, '..', 'dist', 'tsjs-catalog-v1.json');
const releasePath = path.resolve(libDir, '..', 'dist', 'tsjs-release-v1.json');
const SET_NAMES = BUNDLE_SET_NAMES;
const SIZE_NAMES = BUNDLE_SIZE_NAMES;
const TRANSFER_SET_NAMES = Object.freeze(['bootstrap', ...SET_NAMES]);
const HISTORICAL_EVIDENCE_SHA256 =
  '53f762603ad49239f1756171440be422e190cc231efafc56cf37a11e1a38ddf4';
const ROLE_CORRECT_CAPTURE_SHA256 =
  'f1d73d517e4888ef4dc3a84b34166e9aeb6a2bde99dec1c835f151f4e070f64a';
const BOOTSTRAP_BASELINE = Object.freeze({
  rawBytes: 19_101,
  gzipBytes: 5_468,
  brotliBytes: 4_632,
});
const PRODUCTION_SEAM_PATTERN =
  /(?:^|\/)(?:tests?|fixtures?|fakes?|no-?op)(?:\/|$)|(?:^|[/_.-])(?:test|fake|no-?op)(?=[/_.-]|$)|ForTest/u;

function fail(message) {
  throw new Error(`[bundle-budgets] ${message}`);
}

function readJson(file, label) {
  if (!fs.existsSync(file)) fail(`${label} does not exist: ${file}`);
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8'));
  } catch (error) {
    fail(`${label} is not valid JSON (${file}): ${error instanceof Error ? error.message : error}`);
  }
}

function assertPositiveInteger(value, label) {
  if (!Number.isSafeInteger(value) || value <= 0) fail(`${label} must be a positive integer`);
}

function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (value !== null && typeof value === 'object') {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
      .join(',')}}`;
  }
  return JSON.stringify(value);
}

/** Hash JSON with recursive key ordering while preserving array order and values. */
export function canonicalJsonSha256(value) {
  return createHash('sha256').update(canonicalJson(value)).digest('hex');
}

function validateBudgetSets(sets, label) {
  if (!sets || typeof sets !== 'object' || Array.isArray(sets)) {
    fail(`${label} must be an object`);
  }
  for (const setName of SET_NAMES) {
    const set = sets[setName];
    if (!set || typeof set !== 'object' || Array.isArray(set)) {
      fail(`${label}.${setName} must be an object`);
    }
    for (const sizeName of SIZE_NAMES) {
      assertPositiveInteger(set[sizeName], `${label}.${setName}.${sizeName}`);
    }
  }
}

function validateMetricSets(sets, label) {
  validateBudgetSets(sets, label);
  for (const setName of SET_NAMES) {
    const set = sets[setName];
    if (!Array.isArray(set.files) || set.files.length === 0) {
      fail(`${label}.${setName}.files must be a non-empty array`);
    }
    for (const [index, file] of set.files.entries()) {
      if (typeof file !== 'string' || !/^tsjs-[a-z0-9_]+\.js$/.test(file)) {
        fail(`${label}.${setName}.files[${index}] is not a canonical TSJS bundle filename`);
      }
    }
    if (new Set(set.files).size !== set.files.length) {
      fail(`${label}.${setName}.files contains a duplicate`);
    }
    if (typeof set.sha256 !== 'string' || !/^[0-9a-f]{64}$/.test(set.sha256)) {
      fail(`${label}.${setName}.sha256 must be 64 lowercase hexadecimal characters`);
    }
  }
}

/** Validate semantic set membership against exact release artifact ownership. */
export function validateSemanticBundleSets(metrics, release, catalog) {
  if (catalog?.version !== 1) fail('catalog.version must equal 1');
  if (release?.version !== 1 || !Array.isArray(release.artifacts)) {
    fail('release inventory is invalid');
  }
  if (!metrics?.sets) fail('build metrics sets are missing');
  validateMetricSets(metrics.sets, 'buildMetrics.sets');
  if (
    !metrics.bootstrap ||
    metrics.bootstrap.file !== 'gpt-bootstrap-fallback.js' ||
    typeof metrics.bootstrap.sha256 !== 'string' ||
    !/^[0-9a-f]{64}$/.test(metrics.bootstrap.sha256)
  ) {
    fail('buildMetrics.bootstrap must identify the generated bootstrap exactly once');
  }
  for (const sizeName of SIZE_NAMES) {
    assertPositiveInteger(metrics.bootstrap[sizeName], `buildMetrics.bootstrap.${sizeName}`);
  }
  const expected = deriveSemanticBundleSetIds(catalog.modules);
  const files = new Map();
  const ids = new Set();
  for (const artifact of release.artifacts) {
    if (
      !artifact ||
      typeof artifact !== 'object' ||
      typeof artifact.id !== 'string' ||
      typeof artifact.file !== 'string' ||
      !['bootstrap', 'core', 'integration'].includes(artifact.role) ||
      ids.has(artifact.id) ||
      files.has(artifact.file)
    ) {
      fail('release inventory contains an invalid or duplicate artifact');
    }
    ids.add(artifact.id);
    files.set(artifact.file, artifact);
  }
  const expectedArtifacts = [
    { id: 'bootstrap', role: 'bootstrap', file: 'gpt-bootstrap-fallback.js' },
    { id: 'core', role: 'core', file: 'tsjs-core.js' },
    ...catalog.modules.map(({ id }) => ({
      id,
      role: 'integration',
      file: `tsjs-${id}.js`,
    })),
  ];
  if (release.artifacts.length !== expectedArtifacts.length) {
    fail('release inventory does not contain the exact catalog artifact count');
  }
  for (const [index, expectedArtifact] of expectedArtifacts.entries()) {
    const artifact = release.artifacts[index];
    if (
      artifact.id !== expectedArtifact.id ||
      artifact.role !== expectedArtifact.role ||
      artifact.file !== expectedArtifact.file
    ) {
      fail(
        `release inventory artifact ${index} must be ${expectedArtifact.role}/${expectedArtifact.id}/${expectedArtifact.file}`
      );
    }
  }

  const actual = {};
  for (const setName of SET_NAMES) {
    const setIds = metrics.sets[setName].files.map((file) => {
      const artifact = files.get(file);
      if (!artifact || artifact.role === 'bootstrap') {
        fail(`buildMetrics.sets.${setName} contains an unknown or bootstrap artifact`);
      }
      return artifact.id;
    });
    if (new Set(setIds).size !== setIds.length) {
      fail(`buildMetrics.sets.${setName} contains a multiply counted artifact`);
    }
    if (JSON.stringify(setIds) !== JSON.stringify(expected[setName])) {
      fail(
        `buildMetrics.sets.${setName} has semantic ids ${JSON.stringify(setIds)}; expected ${JSON.stringify(expected[setName])}`
      );
    }
    actual[setName] = setIds;
  }
  return actual;
}

function canonicalSourcePath(file) {
  return typeof file === 'string' ? file.replaceAll('\\', '/') : '';
}

function validateSourceOwners(sourceOwners, release) {
  if (!sourceOwners || typeof sourceOwners !== 'object' || Array.isArray(sourceOwners)) {
    fail('captured sourceOwners must be an object');
  }
  const artifactIds = new Set(release.artifacts.map(({ id }) => id));
  const ownersBySource = new Map();
  for (const [source, owners] of Object.entries(sourceOwners)) {
    if (
      source !== canonicalSourcePath(source) ||
      !source.startsWith('src/') ||
      !Array.isArray(owners) ||
      owners.length === 0 ||
      new Set(owners).size !== owners.length ||
      owners.some((owner) => !artifactIds.has(owner))
    ) {
      fail(`captured source ownership is invalid: ${source}`);
    }
    ownersBySource.set(source, new Set(owners));
  }
  return ownersBySource;
}

function readCurrentSourceGraph(metrics, release) {
  if (!Array.isArray(metrics?.modules) || !Array.isArray(release?.artifacts)) {
    fail('build metrics module graph or release inventory is missing');
  }
  const productionArtifacts = release.artifacts.filter(({ role }) => role !== 'bootstrap');
  if (metrics.modules.length !== productionArtifacts.length) {
    fail('build metrics module graph does not classify every production artifact exactly once');
  }
  const rawEntries = [
    {
      artifact: release.artifacts.find(({ role }) => role === 'bootstrap'),
      module: metrics.bootstrap,
    },
    ...metrics.modules.map((module, index) => ({
      artifact: productionArtifacts[index],
      module,
    })),
  ];
  const graphEntries = [];
  const ownersBySource = new Map();
  for (const [index, { artifact, module }] of rawEntries.entries()) {
    if (
      !artifact ||
      !module ||
      typeof module !== 'object' ||
      module.file !== artifact.file ||
      typeof module.entry !== 'string' ||
      !Array.isArray(module.sources)
    ) {
      fail(`build metrics module graph entry ${index} is invalid or out of release order`);
    }
    const entry = canonicalSourcePath(module.entry);
    const sources = new Set();
    for (const [sourceIndex, source] of module.sources.entries()) {
      const sourceFile = canonicalSourcePath(source?.file);
      if (
        !sourceFile.startsWith('src/') ||
        !Number.isSafeInteger(source?.renderedBytes) ||
        source.renderedBytes < 0 ||
        sources.has(sourceFile)
      ) {
        fail(`build metrics module graph ${module.file}.sources[${sourceIndex}] is invalid`);
      }
      sources.add(sourceFile);
      const owners = ownersBySource.get(sourceFile) ?? [];
      if (owners.includes(artifact.id)) {
        fail(`build metrics source ${sourceFile} repeats owner ${artifact.id}`);
      }
      owners.push(artifact.id);
      ownersBySource.set(sourceFile, owners);
    }
    if (artifact.role !== 'bootstrap' && (sources.size === 0 || !sources.has(entry))) {
      fail(`build metrics module graph ${module.file} does not contain its entry source`);
    }
    graphEntries.push({ artifact, module, entry, sources });
  }
  return {
    graphEntries,
    sourceOwners: Object.fromEntries(ownersBySource),
  };
}

function findCriticalDeferredViolations(graphEntries, ownersBySource, release) {
  const artifactsById = new Map(release.artifacts.map((artifact) => [artifact.id, artifact]));
  const violations = [];
  for (const { artifact, sources } of graphEntries) {
    if (artifact.role !== 'core' && artifact.phase !== 'critical') continue;
    for (const source of sources) {
      const owners = ownersBySource.get(source);
      if (owners && [...owners].every((owner) => artifactsById.get(owner)?.phase === 'deferred')) {
        violations.push(`${artifact.id} reaches deferred-owned source ${source}`);
      }
    }
  }
  return violations;
}

/** Return critical artifacts that transitively bundle deferred-owned source. */
export function findCriticalDeferredSourceViolations(metrics, release, sourceOwners) {
  const { graphEntries } = readCurrentSourceGraph(metrics, release);
  const ownersBySource = validateSourceOwners(sourceOwners, release);
  return findCriticalDeferredViolations(graphEntries, ownersBySource, release);
}

/** Return all frozen production graph ownership and seam violations. */
export function findProductionGraphViolations(metrics, release, sourceOwners) {
  const currentGraph = readCurrentSourceGraph(metrics, release);
  const ownersBySource = validateSourceOwners(sourceOwners, release);
  const violations = findCriticalDeferredViolations(
    currentGraph.graphEntries,
    ownersBySource,
    release
  );
  if (canonicalJson(currentGraph.sourceOwners) !== canonicalJson(sourceOwners)) {
    violations.push('current source ownership differs from immutable capture');
  }
  const providersByCapability = new Map();
  for (const { artifact } of currentGraph.graphEntries) {
    for (const capability of artifact.outputs) {
      if (providersByCapability.has(capability)) {
        fail(`capability ${capability} has multiple release providers`);
      }
      providersByCapability.set(capability, { artifact });
    }
  }
  for (const { artifact, sources } of currentGraph.graphEntries) {
    const requiredProviders = new Map();
    for (const input of artifact.inputs) {
      const capability = input.split('?', 1)[0];
      const provider = providersByCapability.get(capability);
      if (provider && provider.artifact.id !== artifact.id) {
        requiredProviders.set(provider.artifact.id, provider);
      }
    }
    for (const source of sources) {
      const canonicalOwners = ownersBySource.get(source);
      let hasSpecificOwnerViolation = false;
      for (const { artifact: providerArtifact } of requiredProviders.values()) {
        if (canonicalOwners?.has(providerArtifact.id) && !canonicalOwners.has(artifact.id)) {
          violations.push(
            `${artifact.id} inlines provider ${providerArtifact.id} implementation ${source}`
          );
          hasSpecificOwnerViolation = true;
        }
      }
      if (PRODUCTION_SEAM_PATTERN.test(source)) {
        violations.push(`${artifact.id} reaches production test/fake/no-op seam ${source}`);
      }
      if (!canonicalOwners) {
        violations.push(`${artifact.id} reaches source without a captured owner ${source}`);
      } else if (!canonicalOwners.has(artifact.id) && !hasSpecificOwnerViolation) {
        violations.push(
          `${artifact.id} includes source owned by ${[...canonicalOwners].join(',')} from ${source}`
        );
      }
    }
  }
  return violations;
}

function validateArtifactContents(release, contents) {
  if (!(contents instanceof Map)) fail('current artifact contents must be a Map');
  const releaseArtifacts = [];
  for (const artifact of release.artifacts) {
    const bytes = contents.get(artifact.file);
    if (!(bytes instanceof Uint8Array))
      fail(`current artifact bytes are missing: ${artifact.file}`);
    const digest = createHash('sha256').update(bytes).digest('hex');
    if (bytes.byteLength !== artifact.bytes || digest !== artifact.hash) {
      fail(`current artifact bytes do not match release inventory: ${artifact.file}`);
    }
    const source = Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength).toString('utf8');
    if (source.includes(RELEASE_SENTINEL) || source.split(release.releaseId).length - 1 !== 1) {
      fail(`current artifact bytes do not contain exactly one release id: ${artifact.file}`);
    }
    releaseArtifacts.push({
      id: artifact.id,
      role: artifact.role,
      phase: artifact.phase ?? '',
      trigger: artifact.trigger ?? '',
      bytes: Buffer.from(source.replace(release.releaseId, RELEASE_SENTINEL)),
    });
  }
  if (computeReleaseId(releaseArtifacts) !== release.releaseId) {
    fail('current artifact bytes do not reproduce release id');
  }
}

function validateCurrentMeasurements(metrics, release, catalog, contents) {
  const expectedSets = deriveInventorySetFiles(release.artifacts, catalog.modules);
  for (const setName of SET_NAMES) {
    const measured = measureBundleSet(expectedSets[setName], contents);
    if (canonicalJson(measured) !== canonicalJson(metrics.sets[setName])) {
      fail(`build metrics do not match current artifact bytes: ${setName}`);
    }
  }
  const bootstrapBytes = contents.get('gpt-bootstrap-fallback.js');
  if (!(bootstrapBytes instanceof Uint8Array)) {
    fail('current artifact bytes are missing: gpt-bootstrap-fallback.js');
  }
  const measuredBootstrap = measureBytes(bootstrapBytes);
  for (const key of [...SIZE_NAMES, 'sha256']) {
    if (metrics.bootstrap[key] !== measuredBootstrap[key]) {
      fail('build metrics do not match current artifact bytes: bootstrap');
    }
  }
  for (const [index, module] of metrics.modules.entries()) {
    const artifact = release.artifacts[index + 1];
    if (module.rawBytes !== artifact.bytes || module.sha256 !== artifact.hash) {
      fail(`build metrics do not match current artifact bytes: ${artifact.id}`);
    }
  }
}

function hasExactKeys(value, keys) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return false;
  const actual = Object.keys(value);
  const expected = new Set(keys);
  return actual.length === expected.size && actual.every((key) => expected.has(key));
}

function validateReleaseInventoryShape(release, label) {
  if (!hasExactKeys(release, ['version', 'releaseId', 'artifacts'])) {
    fail(`${label} release inventory must have exact keys version,releaseId,artifacts`);
  }
  if (
    release.version !== 1 ||
    !/^[0-9a-f]{64}$/u.test(release.releaseId) ||
    !Array.isArray(release.artifacts)
  ) {
    fail(`${label} release inventory has invalid version, releaseId, or artifacts`);
  }
  const artifactKeys = [
    'id',
    'role',
    'phase',
    'trigger',
    'inputs',
    'outputs',
    'file',
    'bytes',
    'hash',
  ];
  const ids = new Set();
  const files = new Set();
  for (const [index, artifact] of release.artifacts.entries()) {
    if (!hasExactKeys(artifact, artifactKeys)) {
      fail(`${label} release artifact ${index} must have exact keys ${artifactKeys.join(',')}`);
    }
    const stringArray = (value) =>
      Array.isArray(value) &&
      value.every((entry) => typeof entry === 'string') &&
      new Set(value).size === value.length;
    const phaseAndTriggerAreValid =
      artifact.role === 'bootstrap' || artifact.role === 'core'
        ? artifact.phase === null && artifact.trigger === null
        : artifact.role === 'integration' &&
          (artifact.phase === 'critical'
            ? artifact.trigger === null
            : artifact.phase === 'deferred' && artifact.trigger === 'first_display_or_idle');
    if (
      !/^[a-z0-9][a-z0-9_-]{0,63}$/u.test(artifact.id) ||
      ids.has(artifact.id) ||
      !phaseAndTriggerAreValid ||
      !stringArray(artifact.inputs) ||
      !stringArray(artifact.outputs) ||
      typeof artifact.file !== 'string' ||
      !/^(?:gpt-bootstrap-fallback|tsjs-[a-z0-9_]+)\.js$/u.test(artifact.file) ||
      files.has(artifact.file) ||
      !Number.isSafeInteger(artifact.bytes) ||
      artifact.bytes <= 0 ||
      !/^[0-9a-f]{64}$/u.test(artifact.hash)
    ) {
      fail(`${label} release artifact ${index} has an invalid shape`);
    }
    ids.add(artifact.id);
    files.add(artifact.file);
  }
}

function validateCurrentReleaseMetadata(current, captured) {
  if (
    current.version !== captured.version ||
    current.artifacts.length !== captured.artifacts.length
  ) {
    fail('current release does not match canonical capture metadata');
  }
  const metadataFields = ['id', 'role', 'phase', 'trigger', 'inputs', 'outputs', 'file'];
  for (const [index, artifact] of current.artifacts.entries()) {
    const currentMetadata = Object.fromEntries(
      metadataFields.map((field) => [field, artifact[field]])
    );
    const capturedMetadata = Object.fromEntries(
      metadataFields.map((field) => [field, captured.artifacts[index]?.[field]])
    );
    if (canonicalJson(currentMetadata) !== canonicalJson(capturedMetadata)) {
      fail(`current release artifact ${index} does not match canonical capture metadata`);
    }
  }
}

function validateCapturedMembership(capture, catalog) {
  const expectedFiles = deriveInventorySetFiles(capture.release.artifacts, catalog.modules);
  const idsByFile = new Map(capture.release.artifacts.map(({ id, file }) => [file, id]));
  const expected = {
    bootstrap: { artifactIds: ['bootstrap'], files: ['gpt-bootstrap-fallback.js'] },
    ...Object.fromEntries(
      SET_NAMES.map((setName) => [
        setName,
        {
          artifactIds: expectedFiles[setName].map((file) => idsByFile.get(file)),
          files: expectedFiles[setName],
        },
      ])
    ),
  };
  for (const setName of TRANSFER_SET_NAMES) {
    const set = capture.sets?.[setName];
    if (
      !set ||
      JSON.stringify(set.artifactIds) !== JSON.stringify(expected[setName].artifactIds) ||
      JSON.stringify(set.files) !== JSON.stringify(expected[setName].files)
    ) {
      fail(`role-correct ${setName} semantic membership is invalid`);
    }
    for (const sizeName of SIZE_NAMES) {
      assertPositiveInteger(set[sizeName], `roleCorrectTransfer.sets.${setName}.${sizeName}`);
    }
    if (!/^[0-9a-f]{64}$/.test(set.sha256)) {
      fail(`roleCorrectTransfer.sets.${setName}.sha256 is invalid`);
    }
  }
}

/** Enforce independent five-percent transfer ceilings with an inclusive ceil boundary. */
export function enforceTransferCeilings(captured, current) {
  const reports = {};
  for (const setName of TRANSFER_SET_NAMES) {
    reports[setName] = {};
    for (const sizeName of SIZE_NAMES) {
      const capturedBytes = captured?.[setName]?.[sizeName];
      const currentBytes = current?.[setName]?.[sizeName];
      assertPositiveInteger(capturedBytes, `captured.${setName}.${sizeName}`);
      assertPositiveInteger(currentBytes, `current.${setName}.${sizeName}`);
      const ceilingBytes = Math.ceil(capturedBytes * 1.05);
      if (currentBytes > ceilingBytes) {
        fail(`${setName}.${sizeName} is ${currentBytes} bytes; ceiling is ${ceilingBytes}`);
      }
      reports[setName][sizeName] = { capturedBytes, currentBytes, ceilingBytes };
    }
  }
  return reports;
}

/** Validate immutable evidence, exact semantics, current artifacts, and transfer ceilings. */
export function validateRoleCorrectTransfer({
  baseline,
  metrics,
  catalog,
  release,
  currentArtifactContents,
  requireExactCapture = false,
}) {
  const capture = baseline?.roleCorrectTransfer;
  if (!capture || typeof capture !== 'object') fail('role-correct capture is missing');
  const historical = Object.fromEntries(
    Object.entries(baseline).filter(([key]) => key !== 'roleCorrectTransfer')
  );
  const historicalDigest = canonicalJsonSha256(historical);
  if (historicalDigest !== HISTORICAL_EVIDENCE_SHA256) {
    fail('historical evidence digest does not match the immutable original top-level fields');
  }
  if (canonicalJsonSha256(capture) !== ROLE_CORRECT_CAPTURE_SHA256) {
    fail('role-correct capture digest does not match the immutable capture');
  }
  if (capture.originalTopLevelSha256 !== HISTORICAL_EVIDENCE_SHA256) {
    fail('historical evidence digest linkage is invalid');
  }

  validateReleaseInventoryShape(capture.release, 'captured');
  validateReleaseInventoryShape(release, 'current');
  validateCapturedMembership(capture, catalog);
  validateCurrentReleaseMetadata(release, capture.release);
  validateSemanticBundleSets(metrics, release, catalog);
  const graphViolations = findProductionGraphViolations(metrics, release, capture.sourceOwners);
  if (graphViolations.length > 0)
    fail(`production graph failed:\n- ${graphViolations.join('\n- ')}`);
  if (requireExactCapture && JSON.stringify(release) !== JSON.stringify(capture.release)) {
    fail('generated release inventory differs from the clean capture parent');
  }
  if (currentArtifactContents !== undefined) {
    validateArtifactContents(release, currentArtifactContents);
    validateCurrentMeasurements(metrics, release, catalog, currentArtifactContents);
  }

  const currentSets = { bootstrap: metrics.bootstrap, ...metrics.sets };
  return {
    reports: enforceTransferCeilings(capture.sets, currentSets),
    capture,
  };
}

function parseArgs(argv) {
  const options = { baselineOnly: false, baselinePath: defaultBaselinePath };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--baseline-only') {
      options.baselineOnly = true;
    } else if (argument === '--baseline') {
      const value = argv[index + 1];
      if (!value) fail('--baseline requires a path');
      options.baselinePath = path.resolve(value);
      index += 1;
    } else {
      fail(`unknown argument: ${argument}`);
    }
  }
  return options;
}

export function checkBundleBudgets({
  baselineOnly = false,
  baselinePath = defaultBaselinePath,
} = {}) {
  const baseline = readJson(baselinePath, 'baseline');
  const metrics = readJson(metricsPath, 'build metrics');
  const catalog = readJson(catalogPath, 'release catalog');
  const release = readJson(releasePath, 'release inventory');
  if (baseline.schemaVersion !== 1) fail('baseline.schemaVersion must equal 1');
  if (metrics.schemaVersion !== 1) fail('build metrics schemaVersion must equal 1');
  validateBudgetSets(baseline.bundles, 'baseline.bundles');
  validateSemanticBundleSets(metrics, release, catalog);

  const failures = findProductionGraphViolations(
    metrics,
    release,
    baseline.roleCorrectTransfer?.sourceOwners
  );
  const historicalDeltas = {};
  const historicalSets = {
    bootstrap: BOOTSTRAP_BASELINE,
    ...Object.fromEntries(SET_NAMES.map((setName) => [setName, baseline.bundles[setName]])),
  };
  const currentSets = { bootstrap: metrics.bootstrap, ...metrics.sets };
  for (const [setName, historical] of Object.entries(historicalSets)) {
    const current = currentSets[setName];
    historicalDeltas[setName] = Object.fromEntries(
      SIZE_NAMES.map((sizeName) => [
        sizeName,
        {
          historicalBytes: historical[sizeName],
          currentBytes: current[sizeName],
          deltaBytes: current[sizeName] - historical[sizeName],
        },
      ])
    );
    if (baselineOnly) {
      for (const sizeName of SIZE_NAMES) {
        if (current[sizeName] !== historical[sizeName]) {
          failures.push(
            `${setName}.${sizeName} is ${current[sizeName]} bytes; exact historical value is ${historical[sizeName]}`
          );
        }
      }
      if (
        setName !== 'bootstrap' &&
        (JSON.stringify(current.files) !== JSON.stringify(historical.files) ||
          current.sha256 !== historical.sha256)
      ) {
        failures.push(`${setName} differs from the exact pre-change artifact`);
      }
    }
  }

  const currentArtifactContents = new Map(
    release.artifacts.map(({ file }) => [
      file,
      fs.readFileSync(path.join(path.dirname(releasePath), file)),
    ])
  );
  const roleCorrect = validateRoleCorrectTransfer({
    baseline,
    metrics,
    catalog,
    release,
    currentArtifactContents,
  });

  if (failures.length > 0) fail(`budget check failed:\n- ${failures.join('\n- ')}`);

  return {
    baselineOnly,
    baselinePath,
    roleCorrectStatus: 'frozen',
    transferCeilingsEnforced: true,
    historicalDeltas,
    roleCorrectTransfer: roleCorrect.reports,
    sets: metrics.sets,
  };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const result = checkBundleBudgets(parseArgs(process.argv.slice(2)));
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : error}\n`);
    process.exitCode = 1;
  }
}
