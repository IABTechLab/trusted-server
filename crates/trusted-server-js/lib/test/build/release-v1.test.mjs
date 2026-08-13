import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
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
  findCriticalDeferredSourceViolations,
  validateSemanticBundleSets,
} from '../../scripts/check-bundle-budgets.mjs';
import * as bundleBudgets from '../../scripts/check-bundle-budgets.mjs';
import * as bundleMetrics from '../../scripts/bundle-metrics.mjs';

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

function readBuildEvidence() {
  return {
    baseline: JSON.parse(
      fs.readFileSync(
        path.join(libDirectory, 'test/fixtures/performance/aps-tsjs-prechange.json'),
        'utf8'
      )
    ),
    catalog: JSON.parse(
      fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-catalog-v1.json'), 'utf8')
    ),
    metrics: JSON.parse(
      fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-build-metrics-v1.json'), 'utf8')
    ),
    release: JSON.parse(
      fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-release-v1.json'), 'utf8')
    ),
  };
}

function executeGeneratedArtifact(window, file, registrations, { preserveTarget = false } = {}) {
  const target = preserveTarget
    ? window.tsjs
    : Object.freeze({
        _registerIntegration: (registration) => {
          registrations.push(registration);
          return true;
        },
      });
  Object.defineProperty(window, 'tsjs', {
    configurable: true,
    value: target,
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

test('critical transport co-bundles core and render ownership within the approved ceiling', () => {
  const metrics = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-build-metrics-v1.json'), 'utf8')
  );
  const core = metrics.modules.find(({ file }) => file === 'tsjs-core.js');
  const renderRuntime = metrics.modules.find(({ file }) => file === 'tsjs-render_runtime.js');

  assert.ok(core, 'core metrics must exist');
  assert.ok(renderRuntime, 'render_runtime metrics must exist');
  assert.ok(
    core.sources.some(({ file }) => file === 'src/integrations/render_runtime/module.ts'),
    'the core transport must co-bundle the mandatory render owner once'
  );
  assert.deepEqual(
    renderRuntime.sources.map(({ file }) => file),
    ['src/integrations/render_runtime/transport_marker.ts'],
    'the logical render artifact must not duplicate the co-bundled implementation'
  );
  assert.ok(metrics.sets.minimal.rawBytes <= 220_000, 'minimal raw bytes exceed the ceiling');
  assert.ok(metrics.sets.minimal.gzipBytes <= 59_000, 'minimal gzip bytes exceed the ceiling');
  assert.ok(
    metrics.sets.minimal.brotliBytes < 51_645,
    'minimal Brotli bytes must improve on the oversized intermediate capture'
  );
});

test('generated integration artifacts execute their release-bound catalog entrypoints', () => {
  const release = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-release-v1.json'), 'utf8')
  );
  const dom = new JSDOM('<!doctype html><html><head></head><body></body></html>', {
    runScripts: 'outside-only',
    url: 'https://publisher.example/article',
  });
  const registrations = [];
  try {
    executeGeneratedArtifact(dom.window, 'tsjs-render_runtime.js', registrations);
    assert.equal(
      registrations.length,
      0,
      'the catalog marker must not publish a duplicate render owner'
    );
    registrations.push({ id: 'render_runtime', phase: 'critical' });
    for (const artifact of release.artifacts.filter(
      ({ role, id }) => role === 'integration' && id !== 'render_runtime'
    )) {
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

test('generated critical transport owns branded render operations without GPT duplication', () => {
  const metrics = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-build-metrics-v1.json'), 'utf8')
  );
  const renderRuntime = metrics.modules.find(({ file }) => file === 'tsjs-render_runtime.js');
  const gpt = metrics.modules.find(({ file }) => file === 'tsjs-gpt.js');

  assert.ok(renderRuntime, 'render_runtime metrics must exist');
  assert.ok(gpt, 'GPT metrics must exist');
  assert.ok(
    metrics.modules
      .find(({ file }) => file === 'tsjs-core.js')
      ?.sources.some(({ file }) => file === 'src/services/render.ts'),
    'the co-bundled critical transport must own the branded render implementation'
  );
  assert.equal(
    gpt.sources.some(({ file }) => file === 'src/services/render.ts'),
    false,
    'GPT must invoke branded operations through render.v1'
  );
  assert.equal(
    gpt.sources.some(({ file }) => file === 'src/core/render.ts'),
    false,
    'GPT must obtain the core-owned iframe constructor through render.v1'
  );
  assert.equal(
    gpt.sources.some(({ file }) => file === 'src/adapters/messaging.ts'),
    false,
    'GPT must consume the core-owned messaging boundary without recompiling it'
  );
  assert.equal(
    metrics.modules
      .find(({ file }) => file === 'tsjs-core.js')
      ?.sources.some(({ file }) => file === 'src/core/puc_shell.ts'),
    false,
    'the GPT-owned PUC shell helper must not inflate the always-on core transport'
  );
  assert.equal(
    gpt.sources.some(({ file }) => file === 'src/core/puc_shell.ts'),
    true,
    'the sole PUC owner must carry its guarded collapsed-shell resize helper'
  );
  for (const source of [
    'src/core/contracts/auction_projection.ts',
    'src/core/contracts/generated/renderer_validator_v1.ts',
    'src/core/contracts/aps_renderer.ts',
    'src/core/config.ts',
    'src/services/projections.ts',
    'src/kernel/identity.ts',
  ]) {
    assert.equal(
      gpt.sources.some(({ file }) => file === source),
      false,
      `GPT must consume core-owned ${source} behavior through capabilities`
    );
  }
});

test('messaging protocol binding is declared before schema initialization', () => {
  const source = fs.readFileSync(path.resolve(libDirectory, 'src/adapters/messaging.ts'), 'utf8');
  const binding =
    "import { TSJS_MESSAGE_PROTOCOL_V1 } from '../kernel/contracts/message_protocol';";
  const initialization = 'export const PROTOCOL_MESSAGE_SCHEMAS_V1';

  assert.ok(source.indexOf(binding) >= 0, 'the messaging protocol must have a local binding');
  assert.ok(
    source.indexOf(binding) < source.indexOf(initialization),
    'the messaging protocol binding must precede schema initialization'
  );
});

test('co-bundled render_runtime and independent GPT start one branded display flow', async () => {
  const release = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-release-v1.json'), 'utf8')
  );
  const dom = new JSDOM(
    '<!doctype html><html><head></head><body><div id="slot-one"></div></body></html>',
    {
      runScripts: 'outside-only',
      url: 'https://publisher.example/article',
    }
  );
  const registrations = [];
  const preparationDisposers = [];
  const activationDisposers = [];
  const displayCalls = [];
  try {
    const targeting = new Map();
    const definedSlots = [];
    const pubads = {
      addEventListener: () => undefined,
      disableInitialLoad: () => undefined,
      getSlots: () => definedSlots,
      refresh: () => undefined,
      removeEventListener: () => undefined,
    };
    dom.window.googletag = {
      apiReady: true,
      pubadsReady: true,
      cmd: { push: (command) => (command(), 1) },
      defineSlot: (adUnitPath, _sizes, elementId) => {
        const slot = {
          addService: () => slot,
          clearTargeting: (key) => {
            if (key === undefined) targeting.clear();
            else targeting.delete(key);
            return slot;
          },
          getAdUnitPath: () => adUnitPath,
          getSlotElementId: () => elementId,
          getTargeting: (key) => targeting.get(key) ?? [],
          setTargeting: (key, value) => {
            targeting.set(key, typeof value === 'string' ? [value] : [...value]);
            return slot;
          },
        };
        definedSlots.push(slot);
        return slot;
      },
      destroySlots: () => true,
      display: (elementId) => displayCalls.push(elementId),
      getConfig: () => ({ disableInitialLoad: false }),
      pubads: () => pubads,
      setConfig: () => undefined,
    };
    const criticalBody = fs.readFileSync(
      path.resolve(libDirectory, '../dist/tsjs-core.js'),
      'utf8'
    );
    const criticalHash = createHash('sha256').update(criticalBody).digest('hex');
    const boot = dom.window.eval(`(() => {
      const freeze = Object.freeze;
      const placement = freeze({
        slot: 'slot-one',
        gamUnitPath: '/123/slot-one',
        divId: 'slot-one',
        formats: freeze([freeze([300, 250])]),
        targeting: freeze({})
      });
      const bid = freeze({
        candidateId: 'AAAAAAAAAAAA',
        slot: 'slot-one',
        provider: 'trusted',
        upstreamBidId: 'upstream-one',
        cpm: 2,
        currency: 'USD',
        targeting: freeze({ hb_bidder: 'trusted' }),
        rendererReservationId: 'r1_aaaaaaaaaaaaaaaaaaaaaa',
        renderSource: freeze({
          type: 'adm',
          version: 1,
          adm: '<main>trusted</main>',
          width: 300,
          height: 250
        })
      });
      return freeze({
        abi: 1,
        releaseId: '${release.releaseId}',
        auctionProjection: freeze({
          version: 1,
          auction: freeze({
            version: 1,
            auctionId: 'generated-cross-bundle',
            results: freeze([freeze({
              slot: 'slot-one',
              outcome: 'winner',
              candidateId: 'AAAAAAAAAAAA'
            })])
          }),
          slots: freeze([placement]),
          bids: freeze([bid])
        }),
        creative: freeze({
          version: 1,
          enabled: false,
          clickGuard: false,
          renderGuard: false
        }),
        diagnostics: freeze({
          version: 1,
          renderTraceOverlay: false,
          gpt: freeze({ active: false })
        }),
        manifest: freeze({
          version: 1,
          releaseId: '${release.releaseId}',
          criticalSrc: '/static/tsjs=tsjs-unified.min.js?v=${criticalHash}',
          integrations: freeze([
            freeze({ id: 'render_runtime', phase: 'critical' }),
            freeze({ id: 'gpt', phase: 'critical' })
          ])
        })
      });
    })()`);
    const criticalScript = dom.window.document.createElement('script');
    criticalScript.id = 'trustedserver-js';
    criticalScript.src = `/static/tsjs=tsjs-unified.min.js?v=${criticalHash}`;
    dom.window.document.head.append(criticalScript);
    Object.defineProperty(dom.window, 'tsjs', { configurable: true, value: {} });
    Object.defineProperties(dom.window.tsjs, {
      boot: { configurable: true, enumerable: true, value: boot, writable: true },
      que: { configurable: true, enumerable: true, value: [], writable: true },
    });
    Object.defineProperty(dom.window.document, 'currentScript', {
      configurable: true,
      value: criticalScript,
    });
    dom.window.eval(criticalBody);
    executeGeneratedArtifact(dom.window, 'tsjs-gpt.js', registrations, { preserveTarget: true });
    await new Promise((resolve) => queueMicrotask(resolve));
    assert.equal(
      dom.window.tsjs?._internal?.state,
      'kernel',
      `critical transport should commit: ${JSON.stringify(dom.window.tsjs?._internal)}`
    );
    for (let index = 0; index < 10 && displayCalls.length === 0; index += 1) {
      await new Promise((resolve) => setTimeout(resolve, 0));
    }

    assert.equal(displayCalls.length, 1);
    assert.equal(displayCalls[0], definedSlots[0]);
  } finally {
    activationDisposers.reverse().forEach((release) => release());
    preparationDisposers.reverse().forEach((release) => release());
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

  assert.deepEqual(actualIds, bundleMetrics.deriveSemanticBundleSetIds(catalog.modules));
  assert.equal(metrics.bootstrap.file, 'gpt-bootstrap-fallback.js');
  assert.equal(
    metrics.compression.concatenationSeparator,
    bundleMetrics.BUNDLE_SEPARATOR.toString('utf8')
  );
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

test('bundle metrics has sole ownership of semantic transfer-set derivation', () => {
  const comparatorSource = fs.readFileSync(
    path.join(libDirectory, 'scripts/check-bundle-budgets.mjs'),
    'utf8'
  );

  assert.equal(typeof bundleMetrics.deriveSemanticBundleSetIds, 'function');
  assert.match(
    comparatorSource,
    /import\s*\{[^}]*deriveSemanticBundleSetIds[^}]*\}\s*from '\.\/bundle-metrics\.mjs'/s
  );
  assert.doesNotMatch(comparatorSource, /const REFERENCE_INCLUDE_ORDER|function isCatalogModule/);
  assert.doesNotMatch(comparatorSource, /function deriveSemanticBundleSetIds\s*\(/);
});

test('role-correct budgets use deterministic pure aggregation and compression metrics', () => {
  const metrics = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-build-metrics-v1.json'), 'utf8')
  );
  const catalog = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-catalog-v1.json'), 'utf8')
  );
  const release = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-release-v1.json'), 'utf8')
  );
  const contents = new Map(
    release.artifacts.map(({ file }) => [
      file,
      fs.readFileSync(path.resolve(libDirectory, '../dist', file)),
    ])
  );

  assert.equal(typeof bundleMetrics.deriveInventorySetFiles, 'function');
  assert.equal(typeof bundleMetrics.measureBundleSet, 'function');
  assert.equal(typeof bundleMetrics.measureBytes, 'function');
  assert.deepEqual(
    bundleMetrics.deriveInventorySetFiles(release.artifacts, catalog.modules),
    Object.fromEntries(Object.entries(metrics.sets).map(([name, set]) => [name, set.files]))
  );
  for (const [name, files] of Object.entries(
    bundleMetrics.deriveInventorySetFiles(release.artifacts, catalog.modules)
  )) {
    assert.deepEqual(bundleMetrics.measureBundleSet(files, contents), metrics.sets[name]);
  }
  assert.deepEqual(
    bundleMetrics.measureBytes(contents.get('gpt-bootstrap-fallback.js')),
    Object.fromEntries(
      ['rawBytes', 'gzipBytes', 'brotliBytes', 'sha256'].map((key) => [key, metrics.bootstrap[key]])
    )
  );
});

test('release build has one bundle aggregation and compression measurement owner', () => {
  const buildSource = fs.readFileSync(path.join(libDirectory, 'build-all.mjs'), 'utf8');

  assert.match(
    buildSource,
    /import\s*\{[^}]*deriveInventorySetFiles[^}]*measureBundleSet[^}]*measureBytes[^}]*\}\s*from '\.\/scripts\/bundle-metrics\.mjs'/s
  );
  assert.match(buildSource, /deriveInventorySetFiles\(artifactInventory, releaseCatalog\)/);
  assert.match(buildSource, /measureBundleSet\(/);
  assert.match(buildSource, /measureBytes\(bootstrapBytes\)/);
  assert.doesNotMatch(buildSource, /node:zlib|const separator\s*=|function compress\s*\(/);
  assert.doesNotMatch(buildSource, /function measureBundleSet\s*\(/);
  assert.doesNotMatch(buildSource, /MINIMAL_CRITICAL_IDS|REFERENCE_CRITICAL_IDS/);
});

test('reduced remediation capture appends provenance without changing earlier evidence', () => {
  const baseline = JSON.parse(
    fs.readFileSync(
      path.join(libDirectory, 'test/fixtures/performance/aps-tsjs-prechange.json'),
      'utf8'
    )
  );
  const original = Object.fromEntries(
    Object.entries(baseline).filter(
      ([key]) => key !== 'roleCorrectTransfer' && key !== 'reviewRemediationTransfer'
    )
  );

  assert.ok(baseline.roleCorrectTransfer, 'role-correct capture must be appended');
  assert.deepEqual(baseline.roleCorrectTransfer.source, {
    ref: 'spec/aps-tsjs-resilience-design',
    sha: '4e2c307923b838716d95e2feeebb994a37bb8025',
  });
  assert.equal(
    baseline.roleCorrectTransfer.originalTopLevelSha256,
    createHash('sha256').update(canonicalJson(original)).digest('hex')
  );
  assert.equal(
    baseline.roleCorrectTransfer.originalTopLevelSha256,
    '53f762603ad49239f1756171440be422e190cc231efafc56cf37a11e1a38ddf4'
  );
  assert.equal(
    baseline.roleCorrectTransfer.compression.concatenationSeparator,
    bundleMetrics.BUNDLE_SEPARATOR.toString('utf8')
  );
  assert.ok(
    baseline.reviewRemediationTransfer,
    'review remediation capture must be appended after the immutable intermediate capture'
  );
  assert.deepEqual(baseline.reviewRemediationTransfer.source, {
    ref: 'spec/aps-tsjs-resilience-design',
    sha: '1e783348e408bd1e4a9017ea3428ed19a67e488d',
  });
  assert.equal(
    baseline.reviewRemediationTransfer.originalTopLevelSha256,
    baseline.roleCorrectTransfer.originalTopLevelSha256
  );
  assert.equal(
    baseline.reviewRemediationTransfer.roleCorrectTransferSha256,
    bundleBudgets.canonicalJsonSha256(baseline.roleCorrectTransfer)
  );
  assert.ok(baseline.reviewRemediationTransfer.sets.minimal.rawBytes <= 220_000);
  assert.ok(baseline.reviewRemediationTransfer.sets.minimal.gzipBytes <= 59_000);
  assert.ok(
    baseline.reviewRemediationTransfer.sets.minimal.brotliBytes <
      baseline.roleCorrectTransfer.sets.minimal.brotliBytes
  );
  for (const size of ['rawBytes', 'gzipBytes', 'brotliBytes']) {
    assert.ok(
      baseline.reviewRemediationTransfer.sets.reference[size] <
        baseline.roleCorrectTransfer.sets.reference[size]
    );
    assert.ok(
      baseline.reviewRemediationTransfer.sets.maximal[size] <=
        baseline.roleCorrectTransfer.sets.maximal[size]
    );
  }
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

  const omittedMaximalModule = structuredClone(metrics);
  omittedMaximalModule.sets.maximal.files.pop();
  assert.throws(
    () => validateSemanticBundleSets(omittedMaximalModule, release, catalog),
    /buildMetrics\.sets\.maximal has semantic ids/
  );
});

test('critical bundle graphs exclude deferred entries and transitive presentation sources', () => {
  const { baseline, metrics, release } = readBuildEvidence();
  const sourceOwners = baseline.reviewRemediationTransfer.sourceOwners;
  const cleanMetrics = structuredClone(metrics);
  assert.deepEqual(findCriticalDeferredSourceViolations(cleanMetrics, release, sourceOwners), []);

  const reachesDeferredEntry = structuredClone(cleanMetrics);
  reachesDeferredEntry.modules[0].sources.push({
    file: 'src/integrations/gpt/later.ts',
    renderedBytes: 1,
  });
  assert.deepEqual(
    findCriticalDeferredSourceViolations(reachesDeferredEntry, release, sourceOwners),
    ['core reaches deferred-owned source src/integrations/gpt/later.ts']
  );

  const reachesPresentationHelper = structuredClone(cleanMetrics);
  reachesPresentationHelper.modules[0].sources.push({
    file: 'src/integrations/gpt_diagnostics/overlay.ts',
    renderedBytes: 1,
  });
  assert.deepEqual(
    findCriticalDeferredSourceViolations(reachesPresentationHelper, release, sourceOwners),
    ['core reaches deferred-owned source src/integrations/gpt_diagnostics/overlay.ts']
  );

  const reachesRenderTracePresentation = structuredClone(cleanMetrics);
  reachesRenderTracePresentation.modules[0].sources.push({
    file: 'src/integrations/gpt_diagnostics/presentation.ts',
    renderedBytes: 1,
  });
  assert.deepEqual(
    findCriticalDeferredSourceViolations(reachesRenderTracePresentation, release, sourceOwners),
    ['core reaches deferred-owned source src/integrations/gpt_diagnostics/presentation.ts']
  );
});

test('permanent comparator pins every historical and role-correct evidence subtree', () => {
  const evidence = readBuildEvidence();
  assert.equal(typeof bundleBudgets.validateRoleCorrectTransfer, 'function');
  assert.doesNotThrow(() => bundleBudgets.validateRoleCorrectTransfer(evidence));

  const originalMutations = {
    schemaVersion: (candidate) => (candidate.schemaVersion = 2),
    mode: (candidate) => (candidate.mode = 'changed'),
    source: (candidate) => (candidate.source.sha = 'a'.repeat(40)),
    environment: (candidate) => (candidate.environment.node = 'changed'),
    sampling: (candidate) => (candidate.sampling.warmups += 1),
    bundles: (candidate) => (candidate.bundles.minimal.rawBytes += 1),
    performance: (candidate) => (candidate.performance.bootToFirstDisplayMs.samples[0] += 1),
    evidence: (candidate) => (candidate.evidence.workflowRunId += 1),
  };
  for (const [subtree, mutate] of Object.entries(originalMutations)) {
    const candidate = structuredClone(evidence);
    mutate(candidate.baseline);
    assert.throws(
      () => bundleBudgets.validateRoleCorrectTransfer(candidate),
      /historical evidence digest/,
      `${subtree} mutation must fail`
    );
  }

  const captureMutations = {
    schemaVersion: (candidate) => (candidate.schemaVersion = 2),
    source: (candidate) => (candidate.source.sha = 'a'.repeat(40)),
    originalTopLevelSha256: (candidate) => (candidate.originalTopLevelSha256 = 'a'.repeat(64)),
    tools: (candidate) => (candidate.tools.node = 'changed'),
    compression: (candidate) => (candidate.compression.gzip.level = 8),
    release: (candidate) => (candidate.release.artifacts[0].bytes += 1),
    sourceOwners: (candidate) => candidate.sourceOwners['src/kernel/runtime.ts'].push('gpt'),
    sets: (candidate) => candidate.sets.maximal.artifactIds.pop(),
  };
  for (const [subtree, mutate] of Object.entries(captureMutations)) {
    const candidate = structuredClone(evidence);
    mutate(candidate.baseline.roleCorrectTransfer);
    assert.throws(
      () => bundleBudgets.validateRoleCorrectTransfer(candidate),
      /role-correct capture digest/,
      `${subtree} mutation must fail`
    );
  }

  const remediationMutations = {
    source: (candidate) => (candidate.source.sha = 'b'.repeat(40)),
    roleCorrectTransferSha256: (candidate) =>
      (candidate.roleCorrectTransferSha256 = 'b'.repeat(64)),
    release: (candidate) => (candidate.release.artifacts[1].bytes += 1),
    sourceOwners: (candidate) => candidate.sourceOwners['src/core/index.ts'].push('gpt'),
    logicalProviderSources: (candidate) => candidate.logicalProviderSources.render_runtime.pop(),
    physicalMarkerOwners: (candidate) => (candidate.physicalMarkerOwners.render_runtime = 'gpt'),
    graphReport: (candidate) => (candidate.graphReport.largestContributions[0].renderedBytes += 1),
    sets: (candidate) => (candidate.sets.minimal.gzipBytes += 1),
  };
  for (const [subtree, mutate] of Object.entries(remediationMutations)) {
    const candidate = structuredClone(evidence);
    mutate(candidate.baseline.reviewRemediationTransfer);
    assert.throws(
      () => bundleBudgets.validateRoleCorrectTransfer(candidate),
      /review-remediation capture digest/,
      `${subtree} remediation mutation must fail`
    );
  }
});

test('bundle check verifies the capture source commit reproduces artifact inputs', () => {
  const evidence = readBuildEvidence();

  assert.doesNotThrow(() =>
    bundleBudgets.validateRoleCorrectTransfer({ ...evidence, verifyGitProvenance: true })
  );
  evidence.baseline.reviewRemediationTransfer.source.sha = '0'.repeat(40);
  assert.throws(
    () => bundleBudgets.validateRoleCorrectTransfer({ ...evidence, verifyGitProvenance: true }),
    /review-remediation capture digest/
  );
});

test('capture-exact validation is phase-aware while current bytes stay authoritative', () => {
  const evidence = readBuildEvidence();
  const contents = new Map(
    evidence.release.artifacts.map(({ file }) => [
      file,
      fs.readFileSync(path.resolve(libDirectory, '../dist', file)),
    ])
  );
  assert.doesNotThrow(() =>
    bundleBudgets.validateRoleCorrectTransfer({
      ...evidence,
      currentArtifactContents: contents,
    })
  );
  const isCleanCaptureRelease =
    evidence.release.releaseId === evidence.baseline.reviewRemediationTransfer.release.releaseId;
  const validateExactCapture = () =>
    bundleBudgets.validateRoleCorrectTransfer({
      ...evidence,
      currentArtifactContents: contents,
      requireExactCapture: true,
    });
  if (isCleanCaptureRelease) {
    assert.doesNotThrow(validateExactCapture);
  } else {
    assert.throws(validateExactCapture, /clean capture parent/);
  }

  const changed = structuredClone(evidence);
  changed.release.artifacts[0].hash = 'a'.repeat(64);
  assert.throws(
    () =>
      bundleBudgets.validateRoleCorrectTransfer({
        ...changed,
        currentArtifactContents: contents,
        requireExactCapture: true,
      }),
    /clean capture parent/
  );
  const changedBytes = new Map(contents);
  changedBytes.set('gpt-bootstrap-fallback.js', Buffer.from('changed'));
  assert.throws(
    () =>
      bundleBudgets.validateRoleCorrectTransfer({
        ...evidence,
        currentArtifactContents: changedBytes,
        requireExactCapture: isCleanCaptureRelease,
      }),
    /current artifact bytes/
  );

  const understatedMetrics = structuredClone(evidence);
  understatedMetrics.metrics.sets.minimal.rawBytes -= 1;
  assert.throws(
    () =>
      bundleBudgets.validateRoleCorrectTransfer({
        ...understatedMetrics,
        currentArtifactContents: contents,
      }),
    /build metrics do not match current artifact bytes/
  );

  const changedMetadata = structuredClone(evidence);
  changedMetadata.release.artifacts[2].inputs = [];
  assert.throws(
    () => bundleBudgets.validateRoleCorrectTransfer(changedMetadata),
    /canonical capture metadata/
  );

  const unexpectedReleaseField = structuredClone(evidence);
  unexpectedReleaseField.release.unexpected = true;
  assert.throws(
    () => bundleBudgets.validateRoleCorrectTransfer(unexpectedReleaseField),
    /current release inventory must have exact keys/
  );

  const unexpectedArtifactField = structuredClone(evidence);
  unexpectedArtifactField.release.artifacts[0].unexpected = true;
  assert.throws(
    () => bundleBudgets.validateRoleCorrectTransfer(unexpectedArtifactField),
    /current release artifact 0 must have exact keys/
  );
});

test('source ownership capture rejects an omitted current non-entry module source', () => {
  const evidence = readBuildEvidence();
  const currentArtifactContents = new Map(
    evidence.release.artifacts.map(({ file }) => [
      file,
      fs.readFileSync(path.resolve(libDirectory, '../dist', file)),
    ])
  );
  const omittedModuleSource = structuredClone(evidence);
  const gptIndex = omittedModuleSource.release.artifacts
    .filter(({ role }) => role !== 'bootstrap')
    .findIndex(({ id }) => id === 'gpt');
  const gptModule = omittedModuleSource.metrics.modules[gptIndex];
  const sourceIndex = gptModule.sources.findIndex(
    ({ file }) => file === 'src/integrations/gpt/startup.ts'
  );
  assert.notEqual(sourceIndex, -1);
  assert.notEqual(gptModule.sources[sourceIndex].file, gptModule.entry);
  gptModule.sources.splice(sourceIndex, 1);
  assert.throws(
    () =>
      bundleBudgets.validateRoleCorrectTransfer({
        ...omittedModuleSource,
        currentArtifactContents,
      }),
    /current source ownership differs from immutable capture/
  );
});

test('source ownership capture rejects cleared current bootstrap sources', () => {
  const evidence = readBuildEvidence();
  const currentArtifactContents = new Map(
    evidence.release.artifacts.map(({ file }) => [
      file,
      fs.readFileSync(path.resolve(libDirectory, '../dist', file)),
    ])
  );
  evidence.metrics.bootstrap.sources = [];
  assert.throws(
    () => bundleBudgets.validateRoleCorrectTransfer({ ...evidence, currentArtifactContents }),
    /current source ownership differs from immutable capture/
  );
});

test('source ownership capture pins artifact-owner order', () => {
  const { baseline, metrics, release } = readBuildEvidence();
  const reorderedOwners = structuredClone(baseline.reviewRemediationTransfer.sourceOwners);
  reorderedOwners['src/core/release.ts'].reverse();
  assert.match(
    bundleBudgets.findProductionGraphViolations(metrics, release, reorderedOwners).join('\n'),
    /current source ownership differs from immutable capture/
  );
});

test('source ownership graph rejects duplicate bootstrap sources and captured owners', () => {
  const { baseline, metrics, release } = readBuildEvidence();
  const duplicateBootstrapSource = structuredClone(metrics);
  duplicateBootstrapSource.bootstrap.sources.push(
    structuredClone(duplicateBootstrapSource.bootstrap.sources[0])
  );
  assert.throws(
    () =>
      bundleBudgets.findProductionGraphViolations(
        duplicateBootstrapSource,
        release,
        baseline.reviewRemediationTransfer.sourceOwners
      ),
    /gpt-bootstrap-fallback\.js\.sources\[.*\] is invalid/
  );

  const duplicateCapturedOwner = structuredClone(baseline.reviewRemediationTransfer.sourceOwners);
  duplicateCapturedOwner['src/kernel/runtime.ts'].push('core');
  assert.throws(
    () => bundleBudgets.findProductionGraphViolations(metrics, release, duplicateCapturedOwner),
    /captured source ownership is invalid/
  );
});

test('exact release key validation accepts equivalent insertion order', () => {
  const evidence = readBuildEvidence();
  evidence.release = Object.fromEntries(Object.entries(evidence.release).reverse());
  evidence.release.artifacts = evidence.release.artifacts.map((artifact) =>
    Object.fromEntries(Object.entries(artifact).reverse())
  );

  assert.doesNotThrow(() => bundleBudgets.validateRoleCorrectTransfer(evidence));
});

test('transfer ceilings use ceil at a fractional five-percent boundary', () => {
  assert.equal(typeof bundleBudgets.enforceTransferCeilings, 'function');
  const captured = Object.fromEntries(
    ['bootstrap', 'minimal', 'reference', 'maximal'].map((setName) => [
      setName,
      { rawBytes: 10, gzipBytes: 10, brotliBytes: 10 },
    ])
  );
  const atCeiling = structuredClone(captured);
  for (const set of Object.values(atCeiling)) {
    set.rawBytes = 11;
    set.gzipBytes = 11;
    set.brotliBytes = 11;
  }
  assert.doesNotThrow(() => bundleBudgets.enforceTransferCeilings(captured, atCeiling));
  atCeiling.reference.gzipBytes += 1;
  assert.throws(
    () => bundleBudgets.enforceTransferCeilings(captured, atCeiling),
    /reference\.gzipBytes is 12 bytes; ceiling is 11/
  );
});

test('production bundle graphs reject every frozen forbidden edge', () => {
  const { baseline, metrics, release } = readBuildEvidence();
  const sourceOwners = baseline.reviewRemediationTransfer.sourceOwners;
  assert.equal(typeof bundleBudgets.findProductionGraphViolations, 'function');
  assert.deepEqual(
    bundleBudgets.findProductionGraphViolations(
      metrics,
      release,
      sourceOwners,
      baseline.reviewRemediationTransfer.logicalProviderSources,
      baseline.reviewRemediationTransfer.physicalMarkerOwners
    ),
    []
  );
  const rejectSource = (artifactId, file, pattern) => {
    const candidate = structuredClone(metrics);
    const artifactIndex = release.artifacts
      .filter(({ role }) => role !== 'bootstrap')
      .findIndex(({ id }) => id === artifactId);
    candidate.modules[artifactIndex].sources.push({ file, renderedBytes: 1 });
    assert.match(
      bundleBudgets.findProductionGraphViolations(candidate, release, sourceOwners).join('\n'),
      pattern
    );
  };

  rejectSource('gpt', 'src/integrations/render_runtime/module.ts', /inlines provider core/);
  rejectSource('aps', 'src/kernel/runtime.ts', /inlines provider core/);
  rejectSource('gpt', 'src/adapters/prebid.ts', /owned by prebid/);
  rejectSource('aps', 'src/shared/dom_insertion_dispatcher.ts', /owned by .*gpt/);
  rejectSource('aps', 'src/test/fake_adapter.ts', /test\/fake\/no-op seam/);
  const vendoredProvider = structuredClone(metrics);
  vendoredProvider.modules[1].sources.push({
    file: 'node_modules/prebid.js/build/dist/prebid.js',
    renderedBytes: 1,
  });
  assert.throws(
    () => bundleBudgets.findProductionGraphViolations(vendoredProvider, release, sourceOwners),
    /sources\[.*\] is invalid/
  );
});

test('production bundle graphs scan bootstrap sources for test and fake seams', () => {
  const { baseline, metrics, release } = readBuildEvidence();
  metrics.bootstrap.sources.push({ file: 'src/test/fake_adapter.ts', renderedBytes: 1 });

  assert.match(
    bundleBudgets
      .findProductionGraphViolations(
        metrics,
        release,
        baseline.reviewRemediationTransfer.sourceOwners
      )
      .join('\n'),
    /bootstrap reaches production test\/fake\/no-op seam src\/test\/fake_adapter\.ts/
  );
});

test('production bundle graphs reject provider implementation modules, not only entries', () => {
  const { baseline, metrics, release } = readBuildEvidence();
  const gptIndex = release.artifacts
    .filter(({ role }) => role !== 'bootstrap')
    .findIndex(({ id }) => id === 'gpt');
  metrics.modules[gptIndex].sources.push({
    file: 'src/integrations/render_runtime/module.ts',
    renderedBytes: 1,
  });

  assert.match(
    bundleBudgets
      .findProductionGraphViolations(
        metrics,
        release,
        baseline.reviewRemediationTransfer.sourceOwners
      )
      .join('\n'),
    /gpt inlines provider core.*src\/integrations\/render_runtime\/module\.ts/
  );
});

test('logical provider ownership cannot be authorized by a duplicated source capture', () => {
  const { baseline, metrics, release } = readBuildEvidence();
  const capture = baseline.reviewRemediationTransfer;
  const providerSource = 'src/services/render.ts';
  const gptIndex = release.artifacts
    .filter(({ role }) => role !== 'bootstrap')
    .findIndex(({ id }) => id === 'gpt');
  metrics.modules[gptIndex].sources.push({ file: providerSource, renderedBytes: 1 });
  const permissiveOwners = structuredClone(capture.sourceOwners);
  permissiveOwners[providerSource] = ['core', 'gpt'];
  const logicalProviderSources = {
    ...capture.logicalProviderSources,
    render_runtime: capture.logicalProviderSources.render_runtime.filter(
      (source) => source !== providerSource
    ),
    core: [...capture.logicalProviderSources.core, providerSource],
  };

  assert.match(
    bundleBudgets
      .findProductionGraphViolations(
        metrics,
        release,
        permissiveOwners,
        logicalProviderSources,
        capture.physicalMarkerOwners
      )
      .join('\n'),
    /gpt inlines provider core.*src\/services\/render\.ts/
  );
});

test('bundle graph report freezes largest contributions and repeated attributions', () => {
  const { metrics, release } = readBuildEvidence();
  const report = bundleBudgets.buildProductionGraphReport(metrics, release);

  assert.equal(report.largestContributions.length, 20);
  assert.equal(report.largestContributions[0].source, 'src/services/slots.ts');
  assert.ok(report.repeatedAttributions.some(({ source }) => source === 'src/core/release.ts'));
});

for (const [consumerId, providerId, providerSource] of [
  ['gpt_later', 'gpt', 'src/integrations/gpt/module.ts'],
  ['osano_lifecycle', 'osano_consent', 'src/integrations/osano/consent.ts'],
  ['prebid_later', 'prebid', 'src/integrations/prebid/module.ts'],
  ['sourcepoint_lifecycle', 'sourcepoint_consent', 'src/integrations/sourcepoint/consent.ts'],
  ['gpt_later', 'gpt', 'src/integrations/gpt/startup.ts'],
  ['prebid_later', 'prebid', 'src/integrations/prebid/startup.ts'],
  ['diagnostics_presentation', 'gpt_diagnostics', 'src/integrations/gpt_diagnostics/store.ts'],
]) {
  test(`production bundle graph rejects ${consumerId} inlining ${providerId} implementation`, () => {
    const { baseline, metrics, release } = readBuildEvidence();
    const artifactIndex = release.artifacts
      .filter(({ role }) => role !== 'bootstrap')
      .findIndex(({ id }) => id === consumerId);
    metrics.modules[artifactIndex].sources.push({ file: providerSource, renderedBytes: 1 });

    assert.match(
      bundleBudgets
        .findProductionGraphViolations(
          metrics,
          release,
          baseline.reviewRemediationTransfer.sourceOwners
        )
        .join('\n'),
      new RegExp(
        `${consumerId} inlines provider ${providerId}.*${providerSource.replaceAll('.', '\\.')}`
      )
    );
  });
}

test('critical bundle graph rejects deferred-presentation-only source ownership', () => {
  const { baseline, metrics, release } = readBuildEvidence();
  const coreIndex = release.artifacts
    .filter(({ role }) => role !== 'bootstrap')
    .findIndex(({ id }) => id === 'core');
  metrics.modules[coreIndex].sources.push({
    file: 'src/integrations/gpt_diagnostics/exhaustive.ts',
    renderedBytes: 1,
  });

  assert.match(
    bundleBudgets
      .findProductionGraphViolations(
        metrics,
        release,
        baseline.reviewRemediationTransfer.sourceOwners
      )
      .join('\n'),
    /core reaches deferred-owned source src\/integrations\/gpt_diagnostics\/exhaustive\.ts/
  );
});

test('production bundle graphs reject actual underscore-named test seams', () => {
  const { baseline, metrics, release } = readBuildEvidence();
  const apsIndex = release.artifacts
    .filter(({ role }) => role !== 'bootstrap')
    .findIndex(({ id }) => id === 'aps');
  metrics.modules[apsIndex].sources.push({
    file: 'src/composition/browser_test.ts',
    renderedBytes: 1,
  });

  assert.match(
    bundleBudgets
      .findProductionGraphViolations(
        metrics,
        release,
        baseline.reviewRemediationTransfer.sourceOwners
      )
      .join('\n'),
    /aps reaches production test\/fake\/no-op seam src\/composition\/browser_test\.ts/
  );
});

test('role-correct bundle check reports historical deltas and enforces transfer ceilings', () => {
  const result = checkBundleBudgets();

  assert.equal(result.roleCorrectStatus, 'immutable-intermediate');
  assert.equal(result.reviewRemediationStatus, 'frozen-release-baseline');
  assert.equal(result.transferCeilingsEnforced, true);
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
  for (const [setName, report] of Object.entries(result.reviewRemediationTransfer)) {
    for (const size of ['rawBytes', 'gzipBytes', 'brotliBytes']) {
      assert.equal(report[size].ceilingBytes, Math.ceil(report[size].capturedBytes * 1.05));
      assert.ok(report[size].currentBytes <= report[size].ceilingBytes, `${setName}.${size}`);
    }
  }
});

test('critical render trace source is data-only and guarded against presentation regression', () => {
  const traceSource = fs.readFileSync(path.join(libDirectory, 'src/core/trace.ts'), 'utf8');
  const architectureSource = fs.readFileSync(
    path.join(libDirectory, 'scripts/check-hard-cutover-absence.mjs'),
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
  const releaseStep = workflow.indexOf('run: npm run test:release');
  const budgetStep = workflow.indexOf('run: npm run check:bundle');

  assert.equal(packageJson.scripts['check:bundle'], 'node scripts/check-bundle-budgets.mjs');
  assert.notEqual(buildStep, -1);
  assert.ok(releaseStep > buildStep, 'release verification must run after the TSJS build');
  assert.ok(budgetStep > buildStep, 'bundle budget check must run after the TSJS build');
  assert.ok(budgetStep > releaseStep, 'bundle budget check must run after release verification');
});

test('hard-cutover absence is exposed once and enforced after both production builds', () => {
  const packageJson = JSON.parse(fs.readFileSync(path.join(libDirectory, 'package.json'), 'utf8'));
  const workflow = fs.readFileSync(path.join(repositoryRoot, '.github/workflows/test.yml'), 'utf8');
  const buildStep = workflow.indexOf('run: npm run build');
  const externalPrebidStep = workflow.indexOf('run: npm run build:prebid-external');
  const absenceStep = workflow.indexOf('run: npm run check:hard-cutover-absence');

  assert.equal(
    packageJson.scripts['check:hard-cutover-absence'],
    'node scripts/check-hard-cutover-absence.mjs'
  );
  assert.equal(
    packageJson.scripts['check:architecture'],
    packageJson.scripts['check:hard-cutover-absence'],
    'architecture and absence commands must share one policy implementation'
  );
  assert.notEqual(buildStep, -1);
  assert.ok(externalPrebidStep > buildStep, 'pure Prebid must build after the TSJS release');
  assert.ok(absenceStep > externalPrebidStep, 'absence must run after both production builds');
});

test('registered integration dispatch selects post-switch evidence without changing the instrument', () => {
  const workflow = fs.readFileSync(
    path.join(repositoryRoot, '.github/workflows/integration-tests.yml'),
    'utf8'
  );

  assert.match(
    workflow,
    /mode: \$\{\{ startsWith\(inputs\.evidence_id, 'aps-tsjs-postswitch-'\) && 'postswitch' \|\| 'preswitch' \}\}/
  );
});

test('protected real-GAM evidence is dispatchable for an unmerged branch without duplicating the suite', () => {
  const integrationWorkflow = fs.readFileSync(
    path.join(repositoryRoot, '.github/workflows/integration-tests.yml'),
    'utf8'
  );
  const realGamWorkflow = fs.readFileSync(
    path.join(repositoryRoot, '.github/workflows/aps-real-gam.yml'),
    'utf8'
  );
  assert.match(realGamWorkflow, /workflow_call:/);
  assert.doesNotMatch(integrationWorkflow, /real_gam_evidence_id:/);
  assert.match(
    integrationWorkflow,
    /real-gam-attestation:[\s\S]*?startsWith\(inputs\.evidence_id, 'aps-tsjs-cutover-'\)[\s\S]*?uses: \.\/\.github\/workflows\/aps-real-gam\.yml/
  );
  assert.match(
    integrationWorkflow,
    /real-gam-attestation:[\s\S]*?evidence_id: \$\{\{ inputs\.evidence_id \}\}/
  );
});

test('cutover workflows bind exact release evidence and prior artifact provenance', () => {
  const qualityWorkflow = fs.readFileSync(
    path.join(repositoryRoot, '.github/workflows/test.yml'),
    'utf8'
  );
  const integrationWorkflow = fs.readFileSync(
    path.join(repositoryRoot, '.github/workflows/integration-tests.yml'),
    'utf8'
  );
  const realGamWorkflow = fs.readFileSync(
    path.join(repositoryRoot, '.github/workflows/aps-real-gam.yml'),
    'utf8'
  );
  const realGamNetwork = fs.readFileSync(
    path.join(
      repositoryRoot,
      'crates/trusted-server-integration-tests/browser/helpers/gam-test-network.ts'
    ),
    'utf8'
  );
  const hardCutoverPolicy = fs.readFileSync(
    path.join(
      repositoryRoot,
      'crates/trusted-server-js/lib/scripts/check-hard-cutover-absence.mjs'
    ),
    'utf8'
  );

  for (const [name, workflow] of [
    ['quality', qualityWorkflow],
    ['integration', integrationWorkflow],
    ['real-GAM', realGamWorkflow],
  ]) {
    assert.match(workflow, /evidence_id:[\s\S]*?required: true/, `${name} evidence id`);
    assert.match(workflow, /release_id:[\s\S]*?required: true/, `${name} release id`);
    assert.match(workflow, /evidence-manifest\.json/, `${name} evidence manifest`);
    assert.match(workflow, /commitSha/, `${name} commit SHA binding`);
    assert.match(workflow, /runId/, `${name} run id binding`);
    assert.match(workflow, /conclusion/, `${name} conclusion binding`);
  }
  for (const [name, workflow] of [
    ['integration', integrationWorkflow],
    ['real-GAM', realGamWorkflow],
  ]) {
    assert.match(workflow, /previous_artifact_id:[\s\S]*?required: true/, `${name} prior input`);
    assert.match(workflow, /previousArtifactId/, `${name} prior artifact binding`);
  }
  assert.match(qualityWorkflow, /aps-tsjs-quality-\$\{\{ github\.run_id \}\}/);
  assert.match(qualityWorkflow, /set -euo pipefail[\s\S]*?quality\.log/);
  assert.match(qualityWorkflow, /tsjs-build-metrics-v1\.json/);
  assert.match(integrationWorkflow, /aps-tsjs-cutover-\$\{\{ github\.sha \}\}/);
  assert.match(integrationWorkflow, /for runtime in axum fastly cloudflare spin/);
  assert.match(integrationWorkflow, /aps-proxy-\$runtime\.log/);
  assert.match(integrationWorkflow, /--project=chromium --project=firefox --project=webkit/);
  assert.match(integrationWorkflow, /Scrub all integration evidence before upload/);
  assert.match(realGamWorkflow, /aps-real-gam-\$\{\{ github\.run_id \}\}/);
  assert.match(realGamWorkflow, /capabilities\?/);
  assert.match(realGamNetwork, /pucRelease\.value !== expectedPucRelease/);
  assert.match(hardCutoverPolicy, /PUC package is vendored into the local harness/);
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
