import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { JSDOM } from 'jsdom';

import {
  RELEASE_SENTINEL,
  computeReleaseId,
  stampRelease,
  validateStampedRelease,
} from '../../scripts/release-v1.mjs';
import {
  checkBundleBudgets,
  deriveSemanticBundleSetIds,
  findCriticalDeferredSourceViolations,
  validateSemanticBundleSets,
} from '../../scripts/check-bundle-budgets.mjs';

const testDirectory = path.dirname(fileURLToPath(import.meta.url));
const libDirectory = path.resolve(testDirectory, '../..');
const repositoryRoot = path.resolve(libDirectory, '../../..');
const bundle = (id, logical, role = 'integration', phase = 'critical', trigger = '') => ({
  id,
  role,
  phase,
  trigger,
  bytes: Buffer.from(`${logical}${RELEASE_SENTINEL}`),
});

const EXPECTED_RELEASE_BUNDLE_ORDER = [
  'bootstrap',
  'core',
  'render_runtime',
  'aps',
  'creative',
  'datadome',
  'didomi',
  'google_tag_manager',
  'gpt',
  'gpt_diagnostics',
  'lockr',
  'osano_consent',
  'permutive_context',
  'sourcepoint_consent',
  'prebid',
  'testlight',
  'diagnostics_presentation',
  'gpt_later',
  'osano_lifecycle',
  'permutive_lifecycle',
  'prebid_later',
  'sourcepoint_lifecycle',
];

const CRITICAL_CONSENT_ARTIFACTS = Object.freeze([
  Object.freeze({
    id: 'osano_consent',
    config: undefined,
    capability: 'osano_consent.v1',
  }),
  Object.freeze({
    id: 'permutive_context',
    config: undefined,
    capability: 'permutive_context.v1',
  }),
  Object.freeze({
    id: 'sourcepoint_consent',
    config: Object.freeze({ rewriteSdk: false }),
    capability: 'sourcepoint_consent.v1',
  }),
]);

function executeGeneratedArtifact(window, file, registrations) {
  Object.defineProperty(window, 'tsjs', {
    configurable: true,
    value: Object.freeze({
      _registerIntegration: (registration) => {
        registrations.push(registration);
        return true;
      },
    }),
  });
  window.eval(fs.readFileSync(path.resolve(libDirectory, '../dist', file), 'utf8'));
}

test('generated release inventory pins the server bundle order', () => {
  const manifest = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-release-v1.json'), 'utf8')
  );

  assert.deepEqual(
    manifest.artifacts.map(({ id }) => id),
    EXPECTED_RELEASE_BUNDLE_ORDER
  );
  assert.equal(manifest.artifacts.filter(({ role }) => role === 'bootstrap').length, 1);
  assert.equal(manifest.artifacts.filter(({ role }) => role === 'core').length, 1);
  assert.equal(manifest.artifacts.filter(({ role }) => role === 'integration').length, 20);
  for (const artifact of manifest.artifacts) {
    assert.deepEqual(Object.keys(artifact), [
      'id',
      'role',
      'phase',
      'trigger',
      'inputs',
      'outputs',
      'file',
      'bytes',
      'hash',
    ]);
  }
});

test('generated maximal integration artifacts execute their real catalog entrypoints', () => {
  const release = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-release-v1.json'), 'utf8')
  );
  const dom = new JSDOM('<!doctype html><html><head></head><body></body></html>', {
    runScripts: 'outside-only',
    url: 'https://publisher.example/article',
  });
  const registrations = [];
  try {
    for (const artifact of release.artifacts.filter(({ role }) => role === 'integration')) {
      executeGeneratedArtifact(dom.window, artifact.file, registrations);
    }
    assert.deepEqual(
      registrations.map(({ id }) => id),
      EXPECTED_RELEASE_BUNDLE_ORDER.slice(2)
    );
    assert.deepEqual(
      registrations.map(({ phase }) => phase),
      release.artifacts.filter(({ role }) => role === 'integration').map(({ phase }) => phase)
    );
  } finally {
    dom.window.close();
  }
});

for (const fixture of CRITICAL_CONSENT_ARTIFACTS) {
  test(`generated ${fixture.id} artifact activates through runtime.v1 and publishes ${fixture.capability}`, () => {
    const dom = new JSDOM('<!doctype html><html><head></head><body></body></html>', {
      runScripts: 'outside-only',
      url: 'https://publisher.example/article',
    });
    const registrations = [];
    const preparationDisposers = [];
    const activationDisposers = [];
    const afterCommit = [];
    try {
      executeGeneratedArtifact(dom.window, `tsjs-${fixture.id}.js`, registrations);
      assert.equal(registrations.length, 1);
      const registration = registrations[0];
      const runtime = dom.window.Object.freeze({
        registerAuctionContext: () => () => undefined,
      });
      const config =
        fixture.id === 'sourcepoint_consent'
          ? dom.window.eval('Object.freeze({ rewriteSdk: false })')
          : fixture.config;
      const prepared = registration.prepare(
        dom.window.Object.freeze({
          config,
          interfaces: dom.window.Object.freeze({ 'runtime.v1': runtime }),
          onDispose: (callback) => preparationDisposers.push(callback),
          signal: new dom.window.AbortController().signal,
        })
      );
      assert.deepEqual(Reflect.ownKeys(prepared.interfaces), [fixture.capability]);
      assert.equal(Object.isFrozen(prepared.interfaces[fixture.capability]), true);
      prepared.activate(
        dom.window.Object.freeze({
          afterCommit: (callback) => afterCommit.push(callback),
          onDispose: (callback) => activationDisposers.push(callback),
          signal: new dom.window.AbortController().signal,
        })
      );
      assert.ok(
        activationDisposers.length > 0 || afterCommit.length > 0,
        'real critical activation must acquire or schedule owned behavior'
      );
    } finally {
      activationDisposers.reverse().forEach((release) => release());
      preparationDisposers.reverse().forEach((release) => release());
      dom.window.close();
    }
  });
}

test('bundle metrics use the required five-module reference vector', () => {
  const metrics = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-build-metrics-v1.json'), 'utf8')
  );
  const catalog = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-catalog-v1.json'), 'utf8')
  );
  const release = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-release-v1.json'), 'utf8')
  );

  const idsByFile = new Map(release.artifacts.map(({ id, file }) => [file, id]));
  const actualIds = Object.fromEntries(
    Object.entries(metrics.sets).map(([name, set]) => [
      name,
      set.files.map((file) => idsByFile.get(file)),
    ])
  );

  assert.deepEqual(actualIds, deriveSemanticBundleSetIds(catalog.modules));
  assert.equal(metrics.bootstrap.file, 'gpt-bootstrap-fallback.js');
  for (const size of ['rawBytes', 'gzipBytes', 'brotliBytes']) {
    assert.ok(Number.isSafeInteger(metrics.bootstrap[size]) && metrics.bootstrap[size] > 0);
  }

  assert.deepEqual(metrics.sets.reference.files, [
    'tsjs-core.js',
    'tsjs-render_runtime.js',
    'tsjs-creative.js',
    'tsjs-gpt.js',
    'tsjs-prebid.js',
    'tsjs-datadome.js',
  ]);
});

test('bundle budget membership rejects every noncanonical release inventory shape', () => {
  const metrics = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-build-metrics-v1.json'), 'utf8')
  );
  const catalog = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-catalog-v1.json'), 'utf8')
  );
  const release = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-release-v1.json'), 'utf8')
  );
  const rejectReleaseMutation = (mutate, pattern) => {
    const candidate = structuredClone(release);
    mutate(candidate.artifacts);
    assert.throws(() => validateSemanticBundleSets(metrics, candidate, catalog), pattern);
  };

  assert.doesNotThrow(() => validateSemanticBundleSets(metrics, release, catalog));
  rejectReleaseMutation((artifacts) => {
    artifacts[0].file = 'tsjs-bootstrap.js';
  }, /bootstrap\/bootstrap\/gpt-bootstrap-fallback\.js/);
  rejectReleaseMutation((artifacts) => {
    artifacts[1].id = 'runtime_core';
  }, /core\/core\/tsjs-core\.js/);
  rejectReleaseMutation((artifacts) => {
    artifacts[2].role = 'core';
  }, /integration\/render_runtime\/tsjs-render_runtime\.js/);
  rejectReleaseMutation((artifacts) => {
    artifacts[artifacts.length - 1] = structuredClone(artifacts.at(-2));
  }, /invalid or duplicate artifact/);
  rejectReleaseMutation((artifacts) => {
    artifacts[artifacts.length - 1] = {
      ...artifacts.at(-1),
      id: 'unknown',
      file: 'tsjs-unknown.js',
    };
  }, /sourcepoint_lifecycle/);
  rejectReleaseMutation((artifacts) => artifacts.pop(), /exact catalog artifact count/);

  const multiplyCounted = structuredClone(metrics);
  multiplyCounted.sets.minimal.files.push(multiplyCounted.sets.minimal.files[0]);
  assert.throws(
    () => validateSemanticBundleSets(multiplyCounted, release, catalog),
    /contains a duplicate/
  );
});

test('critical bundle graphs exclude deferred entries and transitive presentation sources', () => {
  const metrics = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-build-metrics-v1.json'), 'utf8')
  );
  const release = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-release-v1.json'), 'utf8')
  );
  const cleanMetrics = structuredClone(metrics);
  assert.deepEqual(findCriticalDeferredSourceViolations(cleanMetrics, release), []);

  const reachesDeferredEntry = structuredClone(cleanMetrics);
  reachesDeferredEntry.modules[0].sources.push({
    file: 'src/integrations/gpt/later.ts',
    renderedBytes: 1,
  });
  assert.deepEqual(findCriticalDeferredSourceViolations(reachesDeferredEntry, release), [
    'core reaches deferred-owned source src/integrations/gpt/later.ts',
  ]);

  const reachesPresentationHelper = structuredClone(cleanMetrics);
  reachesPresentationHelper.modules[0].sources.push({
    file: 'src/integrations/gpt_diagnostics/overlay.ts',
    renderedBytes: 1,
  });
  assert.deepEqual(findCriticalDeferredSourceViolations(reachesPresentationHelper, release), [
    'core reaches deferred-owned source src/integrations/gpt_diagnostics/overlay.ts',
  ]);

  const reachesRenderTracePresentation = structuredClone(cleanMetrics);
  reachesRenderTracePresentation.modules[0].sources.push({
    file: 'src/integrations/gpt_diagnostics/presentation.ts',
    renderedBytes: 1,
  });
  assert.deepEqual(findCriticalDeferredSourceViolations(reachesRenderTracePresentation, release), [
    'core reaches deferred-owned source src/integrations/gpt_diagnostics/presentation.ts',
  ]);
});

test('pre-capture bundle check reports historical deltas without applying old membership ceilings', () => {
  const result = checkBundleBudgets();

  assert.equal(result.roleCorrectStatus, 'pending-capture');
  assert.equal(result.transferCeilingsEnforced, false);
  assert.deepEqual(Object.keys(result.historicalDeltas), [
    'bootstrap',
    'minimal',
    'reference',
    'maximal',
  ]);
  for (const report of Object.values(result.historicalDeltas)) {
    for (const size of ['rawBytes', 'gzipBytes', 'brotliBytes']) {
      assert.equal(
        report[size].deltaBytes,
        report[size].currentBytes - report[size].historicalBytes
      );
    }
  }
});

test('critical render trace source is data-only and guarded against presentation regression', () => {
  const traceSource = fs.readFileSync(path.join(libDirectory, 'src/core/trace.ts'), 'utf8');
  const architectureSource = fs.readFileSync(
    path.join(libDirectory, 'scripts/check-architecture.mjs'),
    'utf8'
  );

  assert.doesNotMatch(
    traceSource,
    /\b(?:Document|HTMLElement|MutationObserver)\b|createElement|getElementById|querySelector|clipboard|data-ts-/
  );
  assert.match(architectureSource, /critical render trace presentation leakage/);
});

test('bundle budgets are exposed through the package and enforced after the CI build', () => {
  const packageJson = JSON.parse(fs.readFileSync(path.join(libDirectory, 'package.json'), 'utf8'));
  const workflow = fs.readFileSync(path.join(repositoryRoot, '.github/workflows/test.yml'), 'utf8');
  const buildStep = workflow.indexOf('run: npm run build');
  const budgetStep = workflow.indexOf('run: npm run check:bundle');

  assert.equal(packageJson.scripts['check:bundle'], 'node scripts/check-bundle-budgets.mjs');
  assert.notEqual(buildStep, -1);
  assert.ok(budgetStep > buildStep, 'bundle budget check must run after the TSJS build');
});

test('release id changes independently with id, role, phase, trigger, bytes, and order', () => {
  const base = [bundle('core', 'a'), bundle('gpt', 'b')];
  assert.notEqual(computeReleaseId(base), computeReleaseId([bundle('changed', 'a'), base[1]]));
  assert.notEqual(computeReleaseId(base), computeReleaseId([bundle('core', 'a', 'core'), base[1]]));
  assert.notEqual(
    computeReleaseId(base),
    computeReleaseId([bundle('core', 'a', 'integration', 'deferred'), base[1]])
  );
  assert.notEqual(
    computeReleaseId(base),
    computeReleaseId([
      bundle('core', 'a', 'integration', 'critical', 'first_display_or_idle'),
      base[1],
    ])
  );
  assert.notEqual(computeReleaseId(base), computeReleaseId([bundle('core', 'changed'), base[1]]));
  assert.notEqual(computeReleaseId(base), computeReleaseId([base[1], base[0]]));
});

test('u64 length framing distinguishes ambiguous concatenations and artifact counts', () => {
  const left = [bundle('a', 'bc'), bundle('d', 'e')];
  const right = [bundle('ab', 'c'), bundle('d', 'e')];
  assert.notEqual(computeReleaseId(left), computeReleaseId(right));
  assert.notEqual(computeReleaseId([bundle('a', 'bc')]), computeReleaseId(left));
});

test('sentinel multiplicity and remnants fail closed', () => {
  assert.throws(() => computeReleaseId([bundle('core', RELEASE_SENTINEL)]), /exactly one/);
  assert.throws(
    () =>
      computeReleaseId([
        {
          id: 'core',
          role: 'core',
          phase: '',
          trigger: '',
          bytes: Buffer.from('none'),
        },
      ]),
    /exactly one/
  );
  assert.throws(() => stampRelease(`${RELEASE_SENTINEL}${RELEASE_SENTINEL}`, 'a'.repeat(64)));
});

test('wrong release and missing bundle fail validation', () => {
  const release = computeReleaseId([bundle('core', 'a')]);
  const stamped = stampRelease(bundle('core', 'a').bytes, release);
  assert.doesNotThrow(() =>
    validateStampedRelease([{ id: 'core', bytes: stamped }], release, ['core'])
  );
  assert.throws(() =>
    validateStampedRelease([{ id: 'core', bytes: stamped }], 'b'.repeat(64), ['core'])
  );
  assert.throws(() =>
    validateStampedRelease([{ id: 'core', bytes: stamped }], release, ['core', 'gpt'])
  );
});
