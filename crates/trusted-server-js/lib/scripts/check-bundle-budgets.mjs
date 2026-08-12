#!/usr/bin/env node

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

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
const SET_NAMES = ['minimal', 'reference', 'maximal'];
const SIZE_NAMES = ['rawBytes', 'gzipBytes', 'brotliBytes'];
const BOOTSTRAP_BASELINE = Object.freeze({
  rawBytes: 19_101,
  gzipBytes: 5_468,
  brotliBytes: 4_632,
});
const REFERENCE_INCLUDE_ORDER = [
  'always',
  'creative_guard',
  'integration:gpt',
  'integration:prebid',
  'integration:datadome',
];
const DEFERRED_PRESENTATION_SOURCES = new Set([
  'src/integrations/gpt_diagnostics/api.ts',
  'src/integrations/gpt_diagnostics/badges.ts',
  'src/integrations/gpt_diagnostics/binding.ts',
  'src/integrations/gpt_diagnostics/overlay.ts',
]);

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

/** Derive the three budget sets from catalog phase and inclusion semantics. */
export function deriveSemanticBundleSetIds(modules) {
  if (!Array.isArray(modules) || modules.length === 0 || !modules.every(isCatalogModule)) {
    fail('catalog.modules must be a non-empty semantic release catalog');
  }
  const catalogIds = modules.map(({ id }) => id);
  if (new Set(catalogIds).size !== catalogIds.length || catalogIds.includes('core')) {
    fail('catalog.modules contains a duplicate or reserved id');
  }
  const critical = modules.filter(({ phase }) => phase === 'critical');
  const reference = REFERENCE_INCLUDE_ORDER.map((include) =>
    critical.filter((module) => module.include === include)
  );
  if (reference.some((matches) => matches.length !== 1)) {
    fail('catalog does not define the reference critical predicates exactly once');
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

/** Return critical artifacts that transitively bundle deferred-owned source. */
export function findCriticalDeferredSourceViolations(metrics, release) {
  if (!Array.isArray(metrics?.modules) || !Array.isArray(release?.artifacts)) {
    fail('build metrics module graph or release inventory is missing');
  }
  const productionArtifacts = release.artifacts.filter(({ role }) => role !== 'bootstrap');
  if (metrics.modules.length !== productionArtifacts.length) {
    fail('build metrics module graph does not classify every production artifact exactly once');
  }
  const graph = new Map();
  for (const [index, module] of metrics.modules.entries()) {
    const artifact = productionArtifacts[index];
    if (
      !module ||
      typeof module !== 'object' ||
      module.file !== artifact.file ||
      typeof module.entry !== 'string' ||
      !Array.isArray(module.sources) ||
      module.sources.length === 0
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
    }
    if (!sources.has(entry)) {
      fail(`build metrics module graph ${module.file} does not contain its entry source`);
    }
    graph.set(artifact.id, { artifact, entry, sources });
  }

  const deferredOwnedSources = new Set(DEFERRED_PRESENTATION_SOURCES);
  for (const { artifact, entry } of graph.values()) {
    if (artifact.phase === 'deferred') deferredOwnedSources.add(entry);
  }

  const violations = [];
  for (const { artifact, sources } of graph.values()) {
    if (artifact.role !== 'core' && artifact.phase !== 'critical') continue;
    for (const source of sources) {
      if (deferredOwnedSources.has(source)) {
        violations.push(`${artifact.id} reaches deferred-owned source ${source}`);
      }
    }
  }
  return violations;
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

  const failures = findCriticalDeferredSourceViolations(metrics, release);
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

  if (baseline.roleCorrectTransfer !== undefined) {
    failures.push(
      'roleCorrectTransfer exists but its permanent Task 18E validator is not installed'
    );
  }

  if (failures.length > 0) fail(`budget check failed:\n- ${failures.join('\n- ')}`);

  return {
    baselineOnly,
    baselinePath,
    roleCorrectStatus: 'pending-capture',
    transferCeilingsEnforced: false,
    historicalDeltas,
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
