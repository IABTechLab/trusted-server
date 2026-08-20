import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
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
  findTakeoverDeferredSourceViolations,
  validateSemanticBundleSets,
} from '../../scripts/check-bundle-budgets.mjs';
import * as bundleBudgets from '../../scripts/check-bundle-budgets.mjs';
import * as bundleMetrics from '../../scripts/bundle-metrics.mjs';
import {
  findCutoverTextViolations,
  findVendorBoundaryViolations,
} from '../../scripts/check-hard-cutover-absence.mjs';

const testDirectory = path.dirname(fileURLToPath(import.meta.url));
const libDirectory = path.resolve(testDirectory, '../..');
const repositoryRoot = path.resolve(libDirectory, '../../..');
const bundle = (id, logical, role = 'integration', phase = 'takeover', trigger = '') => ({
  id,
  role,
  phase,
  trigger,
  bytes: Buffer.from(`${logical}${RELEASE_SENTINEL}`),
});

const EXPECTED_RELEASE_BUNDLE_ORDER = [
  'bootstrap',
  'first_display',
  'aps_initial',
  'creative_initial',
  'datadome_initial',
  'didomi_initial',
  'google_tag_manager_initial',
  'gpt_initial',
  'lockr_initial',
  'osano_initial',
  'permutive_initial',
  'sourcepoint_initial',
  'prebid_initial',
  'testlight_initial',
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

const TAKEOVER_CONSENT_ARTIFACTS = Object.freeze([
  Object.freeze({
    id: 'osano_consent',
    capability: 'osano_consent.v1',
  }),
  Object.freeze({
    id: 'permutive_context',
    capability: 'permutive_context.v1',
  }),
  Object.freeze({
    id: 'sourcepoint_consent',
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

function buildStructurallyValidDescendant(mutateEvidence = () => {}) {
  const evidence = readBuildEvidence();
  const distDirectory = path.resolve(libDirectory, '../dist');
  const logicalContents = new Map(
    evidence.release.artifacts.map(({ file }) => {
      const source = fs.readFileSync(path.join(distDirectory, file), 'utf8');
      return [file, Buffer.from(source.replace(evidence.release.releaseId, RELEASE_SENTINEL))];
    })
  );
  logicalContents.set(
    'tsjs-core.js',
    Buffer.concat([
      logicalContents.get('tsjs-core.js'),
      Buffer.from('\n/* descendant release */\n'),
    ])
  );
  mutateEvidence(evidence);

  const releaseArtifacts = evidence.release.artifacts.map((artifact) => ({
    id: artifact.id,
    role: artifact.role,
    phase: artifact.phase ?? '',
    trigger: artifact.trigger ?? '',
    bytes: logicalContents.get(artifact.file),
  }));
  const releaseId = computeReleaseId(releaseArtifacts);
  const currentArtifactContents = new Map(
    evidence.release.artifacts.map(({ file }) => [
      file,
      Buffer.from(stampRelease(logicalContents.get(file), releaseId)),
    ])
  );

  evidence.release.releaseId = releaseId;
  for (const artifact of evidence.release.artifacts) {
    const bytes = currentArtifactContents.get(artifact.file);
    artifact.bytes = bytes.byteLength;
    artifact.hash = createHash('sha256').update(bytes).digest('hex');
  }
  Object.assign(
    evidence.metrics.bootstrap,
    bundleMetrics.measureBytes(currentArtifactContents.get('tsjs-bootstrap.js'))
  );
  const productionArtifacts = evidence.release.artifacts.filter(({ role }) => role !== 'bootstrap');
  for (const [index, module] of evidence.metrics.modules.entries()) {
    const artifact = productionArtifacts[index];
    module.rawBytes = artifact.bytes;
    module.sha256 = artifact.hash;
  }
  const setFiles = bundleMetrics.deriveInventorySetFiles(
    evidence.release.artifacts,
    evidence.catalog.modules
  );
  for (const [setName, files] of Object.entries(setFiles)) {
    evidence.metrics.sets[setName] = bundleMetrics.measureBundleSet(files, currentArtifactContents);
  }
  return { ...evidence, currentArtifactContents };
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
  assert.equal(manifest.artifacts.filter(({ role }) => role === 'first_display_base').length, 1);
  assert.equal(manifest.artifacts.filter(({ role }) => role === 'first_display_slice').length, 12);
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

test('release id printer validates the complete generated inventory', () => {
  const manifest = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-release-v1.json'), 'utf8')
  );
  const printed = execFileSync(
    process.execPath,
    [path.join(libDirectory, 'scripts/print-release-id.mjs')],
    { encoding: 'utf8' }
  ).trim();

  assert.equal(printed, manifest.releaseId);
});

test('generated first-display components self-register through one authenticated artifact sink', () => {
  const release = JSON.parse(
    fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-release-v1.json'), 'utf8')
  );
  const firstDisplay = release.artifacts.filter(({ phase }) => phase === 'first_display');
  const dom = new JSDOM(
    `<!doctype html><script id="trustedserver-js" src="/static/tsjs=tsjs-first-display.min.js?m=1fff&v=${'b'.repeat(64)}"></script>`,
    {
      runScripts: 'outside-only',
      url: 'https://publisher.example/article',
    }
  );
  const script = dom.window.document.querySelector('script');
  const registrations = [];
  const target = {};
  Object.defineProperty(target, '_registerFirstDisplay', {
    configurable: true,
    enumerable: false,
    value(registration, source) {
      assert.equal(this, target);
      assert.equal(source, script);
      registrations.push(registration);
      return true;
    },
    writable: false,
  });
  Object.defineProperty(dom.window, 'tsjs', {
    configurable: true,
    value: target,
  });
  Object.defineProperty(dom.window.document, 'currentScript', {
    configurable: true,
    value: script,
  });

  try {
    dom.window.eval(
      firstDisplay
        .map(({ file }) => fs.readFileSync(path.resolve(libDirectory, '../dist', file), 'utf8'))
        .join(';\n')
    );
    assert.deepEqual(
      registrations.map(({ id, order }) => ({ id, order })),
      firstDisplay.map(({ id }, index) => ({ id, order: index + 1 }))
    );
    for (const registration of registrations) {
      assert.deepEqual(Reflect.ownKeys(registration), [
        'abi',
        'id',
        'releaseId',
        'order',
        'prepare',
      ]);
      assert.equal(registration.abi, 1);
      assert.equal(registration.releaseId, release.releaseId);
      assert.equal(typeof registration.prepare, 'function');
      assert.equal(Object.isFrozen(registration), true);
    }
    const baseHost = dom.window.eval(
      'Object.freeze({options:Object.freeze({}),sliceBindings:function(){return undefined;}})'
    );
    const base = registrations[0].prepare(baseHost);
    assert.deepEqual(Reflect.ownKeys(base), ['activate', 'sliceHost']);
    assert.equal(Object.isFrozen(base), true);
    assert.deepEqual(Reflect.ownKeys(base.sliceHost), ['activate']);
    assert.equal(Object.isFrozen(base.sliceHost), true);
    for (const registration of registrations.slice(1)) {
      const prepared = registration.prepare(base.sliceHost);
      assert.deepEqual(Reflect.ownKeys(prepared), ['activate']);
      assert.equal(Object.isFrozen(prepared), true);
    }

    dom.window.eval(`
      window.__firstDisplayEvents = [];
      window.__firstDisplayDisposers = [];
      window.__firstDisplayAfterActivate = undefined;
      window.__firstDisplayGptListeners = {};
      window.__firstDisplayTerminal = undefined;
      var slotElement = document.createElement('div');
      slotElement.id = 'slot-1';
      document.body.appendChild(slotElement);
      window.__firstDisplaySlot = {
        addService: function() { return window.__firstDisplaySlot; },
        getSlotElementId: function() { return 'slot-1'; },
        setTargeting: function() { return window.__firstDisplaySlot; }
      };
      window.__firstDisplayPubads = {
        addEventListener: function(name, listener) {
          window.__firstDisplayGptListeners[name] = listener;
        },
        getSlots: function() { return []; },
        refresh: function() {},
        removeEventListener: function() {}
      };
      window.googletag = {
        cmd: {push: function(command) { command(); }},
        defineSlot: function() { return window.__firstDisplaySlot; },
        destroySlots: function() { return true; },
        display: function() {
          window.__firstDisplayEvents.push('gpt:display');
          window.__firstDisplayGptListeners.slotRequested({slot: window.__firstDisplaySlot});
          window.__firstDisplayGptListeners.slotRenderEnded({
            slot: window.__firstDisplaySlot,
            isEmpty: true
          });
        },
        getConfig: function() { return {disableInitialLoad: false}; },
        pubads: function() { return window.__firstDisplayPubads; }
      };
      window.__firstDisplayBaseHost = Object.freeze({
        options: Object.freeze({
          batch: Object.freeze({
            version: 1,
            projectionDigest: '${'c'.repeat(64)}',
            projection: Object.freeze({
              version: 1,
              auction: Object.freeze({
                version: 1,
                auctionId: 'initial',
                results: Object.freeze([Object.freeze({
                  slot: 'slot-1',
                  outcome: 'winner',
                  candidateId: 'candidate001'
                })])
              }),
              slots: Object.freeze([Object.freeze({
                slot: 'slot-1',
                gamUnitPath: '/123/example',
                divId: 'slot-1',
                formats: Object.freeze([Object.freeze([300, 250])]),
                targeting: Object.freeze({placement: 'article'})
              })]),
              bids: Object.freeze([Object.freeze({
                candidateId: 'candidate001',
                slot: 'slot-1',
                provider: 'example',
                upstreamBidId: 'upstream-1',
                cpm: 1.25,
                currency: 'USD',
                targeting: Object.freeze({hb_pb: '1.25'}),
                rendererReservationId: 'r1_${'a'.repeat(22)}',
                renderSource: Object.freeze({
                  type: 'adm',
                  version: 1,
                  adm: '<main>fictional creative</main>',
                  width: 300,
                  height: 250
                })
              })])
            })
          }),
          bootstrap: Object.freeze({
            get state() { return 'agent_registered'; },
            startedAtMs: 0,
            registerAgent: function() {
              window.__firstDisplayEvents.push('bootstrap:register');
              return true;
            },
            startAction: function() {
              window.__firstDisplayEvents.push('bootstrap:action');
              return true;
            },
            settle: function() { return true; },
            fail: function() { return true; }
          }),
          gptInput: Object.freeze({
            browser: window,
            clearTimer: function() {},
            document: document,
            setTimer: function(callback) { return callback; }
          }),
          performance: Object.freeze({mark: function() {}}),
          paint: Object.freeze({
            hidden: function() { return false; },
            requestFrame: function() {},
            scheduleHidden: function() {}
          }),
          onProtectedPaint: function() {},
          onFailure: function() {}
        }),
        sliceBindings: function(id) {
          if (id === 'aps_initial') {
            return Object.freeze({
              bindings: Object.freeze({
                observe: function() {},
                publisherOrigin: 'https://publisher.example',
                register: function(protocol) {
                  window.__firstDisplayEvents.push('aps:' + protocol.id);
                  return function() {};
                }
              }),
              config: Object.freeze({})
            });
          }
          if (id === 'gpt_initial') {
            return Object.freeze({
              bindings: Object.freeze({
                gam: function() { return true; },
                observe: function() {},
                register: function(protocol) {
                  window.__firstDisplayEvents.push('gpt:' + protocol.id);
                  return function() {};
                }
              }),
              config: Object.freeze({gamAttributionEnabled: false, pageBidsEnabled: false})
            });
          }
          return undefined;
        }
      });
      window.__firstDisplayActivation = Object.freeze({
        own: function(dispose) { window.__firstDisplayDisposers.push(dispose); },
        afterActivate: function(callback) { window.__firstDisplayAfterActivate = callback; }
      });
      window.__firstDisplaySliceActivation = Object.freeze({
        own: function(dispose) { window.__firstDisplayDisposers.push(dispose); },
        afterActivate: function() { throw new Error('optional slice cannot start the agent'); }
      });
    `);
    const activatedBase = registrations[0].prepare(dom.window.__firstDisplayBaseHost);
    const activatedGpt = registrations
      .find(({ id }) => id === 'gpt_initial')
      .prepare(activatedBase.sliceHost);
    activatedBase.activate(dom.window.__firstDisplayActivation);
    activatedGpt.activate(dom.window.__firstDisplaySliceActivation);
    dom.window.__firstDisplayAfterActivate();
    const directFrame = dom.window.document.querySelector('#slot-1 iframe');
    assert.ok(directFrame, 'the generated bridge must stage the direct empty-GAM fallback');
    directFrame.dispatchEvent(new dom.window.Event('load'));
    assert.deepEqual(
      [...dom.window.__firstDisplayEvents],
      ['gpt:gpt', 'bootstrap:register', 'bootstrap:action', 'gpt:display']
    );
  } finally {
    dom.window.close();
  }
});

test('takeover transport co-bundles core and render ownership exactly once', () => {
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
    registrations.push({ id: 'render_runtime', phase: 'takeover' });
    for (const artifact of release.artifacts.filter(
      ({ role, id }) => role === 'integration' && id !== 'render_runtime'
    )) {
      executeGeneratedArtifact(dom.window, artifact.file, registrations);
    }
    const integrationArtifacts = release.artifacts.filter(
      ({ role, id }) => role === 'integration' && id !== 'render_runtime'
    );
    for (let index = 0; index < integrationArtifacts.length; index += 1) {
      const artifact = integrationArtifacts[index];
      const registration = registrations[index + 1];
      assert.deepEqual(
        Reflect.ownKeys(registration),
        artifact.phase === 'takeover'
          ? ['abi', 'id', 'phase', 'releaseId', 'prepareSync', 'prepare']
          : ['abi', 'id', 'phase', 'releaseId', 'prepare']
      );
      assert.equal(typeof registration.prepare, 'function');
      assert.equal(
        Object.prototype.hasOwnProperty.call(registration, 'prepareSync'),
        artifact.phase === 'takeover'
      );
    }
    assert.deepEqual(
      registrations.map(({ id }) => id),
      release.artifacts.filter(({ role }) => role === 'integration').map(({ id }) => id)
    );
    assert.deepEqual(
      registrations.map(({ phase }) => phase),
      release.artifacts.filter(({ role }) => role === 'integration').map(({ phase }) => phase)
    );
  } finally {
    dom.window.close();
  }
});

test('generated takeover transport owns branded render operations without GPT duplication', () => {
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
    'the co-bundled takeover transport must own the branded render implementation'
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

test('generated APS bootstrap configuration preserves its public wire keys', () => {
  const source = fs.readFileSync(path.resolve(libDirectory, '../dist/tsjs-aps_initial.js'), 'utf8');

  assert.match(
    source,
    /TS APS Bootstrap Configure",version:2,bootstrapNonce:[^,}]+,["']?rendererNonce["']?:/u,
    'the independently minified APS slice must serialize rendererNonce with its authored name'
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
    const takeoverBody = fs.readFileSync(
      path.resolve(libDirectory, '../dist/tsjs-core.js'),
      'utf8'
    );
    const runtimeHash = createHash('sha256').update(takeoverBody).digest('hex');
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
        integrations: freeze({
          version: 1,
          entries: freeze([freeze({
            id: 'gpt',
            config: freeze({ gamAttributionEnabled: false, pageBidsEnabled: false })
          })])
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
          firstDisplay: null,
          runtimeSrc: '/static/tsjs=tsjs-unified.min.js?v=${runtimeHash}',
          integrations: freeze([
            freeze({ id: 'render_runtime', phase: 'takeover' }),
            freeze({ id: 'gpt', phase: 'takeover' })
          ])
        })
      });
    })()`);
    const runtimeScript = dom.window.document.createElement('script');
    runtimeScript.id = 'trustedserver-js';
    runtimeScript.src = `/static/tsjs=tsjs-unified.min.js?v=${runtimeHash}`;
    dom.window.document.head.append(runtimeScript);
    Object.defineProperty(dom.window, 'tsjs', { configurable: true, value: {} });
    Object.defineProperties(dom.window.tsjs, {
      boot: { configurable: true, enumerable: true, value: boot, writable: true },
      que: { configurable: true, enumerable: true, value: [], writable: true },
      _claimBootSnapshot: {
        configurable: true,
        enumerable: false,
        value: (source) => {
          if (source !== runtimeScript) return undefined;
          Reflect.deleteProperty(dom.window.tsjs, '_claimBootSnapshot');
          return boot;
        },
        writable: false,
      },
    });
    Object.defineProperty(dom.window.document, 'currentScript', {
      configurable: true,
      value: runtimeScript,
    });
    let publisherMicrotaskRan = false;
    dom.window.queueMicrotask(() => {
      publisherMicrotaskRan = true;
    });
    dom.window.eval(takeoverBody);
    executeGeneratedArtifact(dom.window, 'tsjs-gpt.js', registrations, { preserveTarget: true });
    assert.equal(
      dom.window.tsjs?._internal?.state,
      'kernel',
      `takeover transport should commit: ${JSON.stringify(dom.window.tsjs?._internal)}`
    );
    assert.equal(
      publisherMicrotaskRan,
      false,
      'no-agent preparation, activation, and commit must not yield to publisher microtasks'
    );
    await new Promise((resolve) => queueMicrotask(resolve));
    assert.equal(publisherMicrotaskRan, true);
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

for (const fixture of TAKEOVER_CONSENT_ARTIFACTS) {
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
          : dom.window.eval('Object.freeze({})');
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
        'real takeover activation must acquire or schedule owned behavior'
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
  assert.equal(metrics.bootstrap.file, 'tsjs-bootstrap.js');
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

test('bundle metrics enumerate and hash every reachable first-display mask', () => {
  const { metrics } = readBuildEvidence();
  const masks = metrics.firstDisplay.masks;

  assert.ok(Array.isArray(masks));
  assert.equal(masks.length, 2_560);
  assert.equal(new Set(masks.map(({ mask }) => mask)).size, masks.length);
  assert.equal(new Set(masks.map(({ sha256 }) => sha256)).size, masks.length);
  for (const measurement of masks) {
    assert.match(measurement.mask, /^[0-9a-f]{4}$/u);
    assert.equal(measurement.ids[0], 'first_display');
    if (!measurement.ids.includes('gpt_initial')) {
      assert.equal(measurement.ids.includes('aps_initial'), false);
      assert.equal(measurement.ids.includes('prebid_initial'), false);
    }
    assert.deepEqual(
      measurement.files,
      measurement.ids.map((id) => `tsjs-${id}.js`)
    );
    for (const size of ['rawBytes', 'gzipBytes', 'brotliBytes']) {
      assert.ok(Number.isSafeInteger(measurement[size]) && measurement[size] > 0);
    }
    assert.equal(typeof measurement.permitted, 'boolean');
    assert.match(measurement.sha256, /^[0-9a-f]{64}$/u);
  }
});

test('candidate architecture obeys every independent absolute transfer ceiling', () => {
  const { metrics, release, catalog } = readBuildEvidence();
  const currentArtifactContents = new Map(
    release.artifacts.map(({ file }) => [
      file,
      fs.readFileSync(path.resolve(libDirectory, '../dist', file)),
    ])
  );
  const report = bundleBudgets.buildCandidateArchitectureSizeReport({
    metrics,
    release,
    catalog,
    currentArtifactContents,
  });

  assert.doesNotThrow(() => bundleBudgets.enforceCandidateArchitectureSizeCeilings(report));
  assert.equal(report.firstDisplay.masks.length, 2_560);
  assert.ok(report.firstDisplay.permittedMasks.length > 0);
  assert.ok(report.firstDisplay.permittedMasks.length < report.firstDisplay.masks.length);
  for (const name of ['minimal', 'reference', 'aps']) {
    assert.equal(report.firstDisplay.named[name].permitted, true);
  }
  assert.deepEqual(report.firstDisplay.named.reference.ids, [
    'first_display',
    'creative_initial',
    'datadome_initial',
    'gpt_initial',
    'prebid_initial',
  ]);
  assert.deepEqual(report.firstDisplay.named.aps.ids, [
    'first_display',
    'aps_initial',
    'creative_initial',
    'gpt_initial',
  ]);
  assert.deepEqual(Object.keys(report.firstDisplay.named), [
    'minimal',
    'reference',
    'aps',
    'largestRaw',
    'largestGzip',
    'largestBrotli',
  ]);
  assert.deepEqual(Object.keys(report.ceilings), [
    'bootstrap',
    'firstDisplayAgent',
    'referencePersistent',
    'maximalTotal',
  ]);
});

test('absolute transfer ceilings reject independent one-byte regressions', () => {
  const ceilings = bundleBudgets.CANDIDATE_ARCHITECTURE_SIZE_CEILINGS;
  for (const semanticSet of Object.keys(ceilings)) {
    for (const size of ['rawBytes', 'gzipBytes', 'brotliBytes']) {
      const report = Object.fromEntries(
        Object.entries(ceilings).map(([name, limits]) => [name, { ...limits }])
      );
      report[semanticSet][size] += 1;
      assert.throws(
        () => bundleBudgets.enforceCandidateArchitectureSizeCeilings(report),
        new RegExp(`${semanticSet}\\.${size} exceeds`)
      );
    }
  }
});

test('generated mask allowlist must exactly match size-admitted reachable masks', () => {
  const { metrics, release, catalog } = readBuildEvidence();
  const currentArtifactContents = new Map(
    release.artifacts.map(({ file }) => [
      file,
      fs.readFileSync(path.resolve(libDirectory, '../dist', file)),
    ])
  );
  catalog.permittedFirstDisplayMasks.pop();

  assert.throws(
    () =>
      bundleBudgets.buildCandidateArchitectureSizeReport({
        metrics,
        release,
        catalog,
        currentArtifactContents,
      }),
    /generated permitted first-display masks/
  );
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
    bundleMetrics.measureBytes(contents.get('tsjs-bootstrap.js')),
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
  assert.doesNotMatch(buildSource, /MINIMAL_TAKEOVER_IDS|REFERENCE_TAKEOVER_IDS/);
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
    sha: '91b3533ae7c07e03fa77441e0d94f27e31965d9e',
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
    artifacts[0].file = 'gpt-bootstrap-fallback.js';
  }, /bootstrap\/bootstrap\/tsjs-bootstrap\.js/);
  rejectReleaseMutation((artifacts) => {
    artifacts.find(({ id }) => id === 'core').id = 'runtime_core';
  }, /core\/core\/tsjs-core\.js/);
  rejectReleaseMutation((artifacts) => {
    artifacts.find(({ id }) => id === 'render_runtime').role = 'core';
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

test('takeover bundle graphs exclude deferred entries and transitive presentation sources', () => {
  const { metrics, release } = readBuildEvidence();
  const cleanMetrics = structuredClone(metrics);
  assert.deepEqual(findTakeoverDeferredSourceViolations(cleanMetrics, release), []);

  const reachesDeferredEntry = structuredClone(cleanMetrics);
  reachesDeferredEntry.modules
    .find(({ file }) => file === 'tsjs-core.js')
    .sources.push({
      file: 'src/integrations/gpt/later.ts',
      renderedBytes: 1,
    });
  assert.deepEqual(findTakeoverDeferredSourceViolations(reachesDeferredEntry, release), [
    'core reaches deferred-owned source src/integrations/gpt/later.ts',
  ]);

  const reachesPresentationHelper = structuredClone(cleanMetrics);
  reachesPresentationHelper.modules
    .find(({ file }) => file === 'tsjs-core.js')
    .sources.push({
      file: 'src/integrations/gpt_diagnostics/overlay.ts',
      renderedBytes: 1,
    });
  assert.deepEqual(findTakeoverDeferredSourceViolations(reachesPresentationHelper, release), [
    'core reaches deferred-owned source src/integrations/gpt_diagnostics/overlay.ts',
  ]);

  const reachesRenderTracePresentation = structuredClone(cleanMetrics);
  reachesRenderTracePresentation.modules
    .find(({ file }) => file === 'tsjs-core.js')
    .sources.push({
      file: 'src/integrations/gpt_diagnostics/presentation.ts',
      renderedBytes: 1,
    });
  assert.deepEqual(findTakeoverDeferredSourceViolations(reachesRenderTracePresentation, release), [
    'core reaches deferred-owned source src/integrations/gpt_diagnostics/presentation.ts',
  ]);
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

test('bundle check authenticates historical capture provenance for descendant builds', () => {
  const evidence = readBuildEvidence();
  const intermediate = evidence.baseline.roleCorrectTransfer;
  const capture = evidence.baseline.reviewRemediationTransfer;

  assert.doesNotThrow(() => bundleBudgets.validateFrozenCaptureProvenance(intermediate, capture));
  assert.doesNotThrow(() => bundleBudgets.validateCaptureSourceProvenance(capture));
  assert.doesNotThrow(() =>
    bundleBudgets.validateRoleCorrectTransfer({ ...evidence, verifyGitProvenance: true })
  );
});

test('capture provenance rejects mismatched recorded lock and tool metadata', () => {
  const baseline = readBuildEvidence().baseline;
  const mutations = {
    packageLockSha256: (candidate) => (candidate.tools.packageLockSha256 = '0'.repeat(64)),
    node: (candidate) => (candidate.tools.node = 'v0.0.0'),
    npm: (candidate) => (candidate.tools.npm = '0.0.0'),
    typescript: (candidate) => (candidate.tools.typescript = '0.0.0'),
    vite: (candidate) => (candidate.tools.vite = '0.0.0'),
    esbuild: (candidate) => (candidate.tools.esbuild = '0.0.0'),
  };

  for (const captureName of ['roleCorrectTransfer', 'reviewRemediationTransfer']) {
    for (const [field, mutate] of Object.entries(mutations)) {
      const intermediate = structuredClone(baseline.roleCorrectTransfer);
      const capture = structuredClone(baseline.reviewRemediationTransfer);
      mutate(captureName === 'roleCorrectTransfer' ? intermediate : capture);
      assert.throws(
        () => bundleBudgets.validateFrozenCaptureProvenance(intermediate, capture),
        new RegExp(field, 'i'),
        `${captureName}.${field}`
      );
    }
  }
});

test('capture provenance rejects an invalid source or missing captured build input', () => {
  const baseline = readBuildEvidence().baseline;
  const capture = baseline.reviewRemediationTransfer;
  const invalidSource = structuredClone(capture);
  invalidSource.source.sha = '0'.repeat(40);

  assert.throws(
    () => bundleBudgets.validateCaptureSourceProvenance(invalidSource),
    /capture source SHA/
  );
  const invalidIntermediate = structuredClone(baseline.roleCorrectTransfer);
  invalidIntermediate.source.sha = '0'.repeat(40);
  assert.throws(
    () => bundleBudgets.validateFrozenCaptureProvenance(invalidIntermediate, capture),
    /capture source SHA/
  );
  assert.throws(
    () =>
      bundleBudgets.validateCaptureSourceProvenance(capture, {
        head: `${capture.source.sha}^`,
      }),
    /not an ancestor/
  );
  assert.throws(
    () =>
      bundleBudgets.validateCaptureSourceProvenance(capture, {
        buildInputs: ['crates/trusted-server-js/lib/does-not-exist'],
      }),
    /captured build input does not exist/
  );
});

test('authenticated frozen captures report descendant release drift without rejecting it', () => {
  const descendant = buildStructurallyValidDescendant();

  const result = bundleBudgets.validateRoleCorrectTransfer(descendant);

  assert.notEqual(
    descendant.release.releaseId,
    descendant.baseline.reviewRemediationTransfer.release.releaseId
  );
  assert.deepEqual(Object.keys(result.captureReports), [
    'roleCorrectTransfer',
    'reviewRemediationTransfer',
  ]);
  for (const report of Object.values(result.captureReports)) {
    assert.equal(
      report.minimal.rawBytes.deltaBytes,
      report.minimal.rawBytes.currentBytes - report.minimal.rawBytes.capturedBytes
    );
    assert.equal(Object.hasOwn(report.minimal.rawBytes, 'ceilingBytes'), false);
  }

  const coreIndex = descendant.release.artifacts
    .filter(({ role }) => role !== 'bootstrap')
    .findIndex(({ id }) => id === 'core');
  descendant.metrics.modules[coreIndex].sources.push({
    file: 'src/test/descendant_fake.ts',
    renderedBytes: 1,
  });
  assert.throws(
    () => bundleBudgets.validateRoleCorrectTransfer(descendant),
    /production test\/fake\/no-op seam/
  );
  descendant.metrics.modules[coreIndex].sources.pop();
  descendant.release.artifacts.pop();
  assert.throws(
    () => bundleBudgets.validateRoleCorrectTransfer(descendant),
    /live catalog artifact count|exact catalog artifact count/
  );
});

test('valid live catalog and release drift is independent from both frozen captures', () => {
  const authoredCatalog = bundleBudgets.loadAuthoredReleaseCatalog();
  const descendant = buildStructurallyValidDescendant((evidence) => {
    const catalogEntry = evidence.catalog.modules.find(({ id }) => id === 'testlight');
    const artifact = evidence.release.artifacts.find(({ id }) => id === 'testlight');
    const authoredEntry = authoredCatalog.find(({ id }) => id === 'testlight');
    catalogEntry.phase = 'deferred';
    catalogEntry.trigger = 'first_display_or_idle';
    artifact.phase = 'deferred';
    artifact.trigger = 'first_display_or_idle';
    authoredEntry.phase = 'deferred';
    authoredEntry.trigger = 'first_display_or_idle';
  });

  assert.throws(
    () => bundleBudgets.validateRoleCorrectTransfer(descendant),
    /generated catalog entry .* differs from current authored catalog/
  );
  assert.doesNotThrow(() =>
    bundleBudgets.validateRoleCorrectTransfer({ ...descendant, authoredCatalog })
  );
});

test('current release validation is capture-independent while generated bytes stay authoritative', () => {
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
  const changed = structuredClone(evidence);
  changed.release.artifacts[0].hash = 'a'.repeat(64);
  assert.throws(
    () =>
      bundleBudgets.validateRoleCorrectTransfer({
        ...changed,
        currentArtifactContents: contents,
      }),
    /current artifact bytes/
  );
  const changedBytes = new Map(contents);
  changedBytes.set('tsjs-bootstrap.js', Buffer.from('changed'));
  assert.throws(
    () =>
      bundleBudgets.validateRoleCorrectTransfer({
        ...evidence,
        currentArtifactContents: changedBytes,
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
  changedMetadata.release.artifacts.find(({ id }) => id === 'aps').inputs = [];
  assert.throws(
    () => bundleBudgets.validateRoleCorrectTransfer(changedMetadata),
    /must consume runtime\.v1/
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

test('current semantic graph rejects malformed, duplicate, unresolved, late, and deferred capabilities', () => {
  const mutations = [
    [
      /invalid output capability/,
      (candidate) => candidate.release.artifacts.find(({ id }) => id === 'aps').outputs.push('bad'),
    ],
    [
      /multiple providers/,
      (candidate) =>
        candidate.release.artifacts.find(({ id }) => id === 'aps').outputs.push('runtime.v1'),
    ],
    [
      /unknown capability/,
      (candidate) =>
        candidate.release.artifacts.find(({ id }) => id === 'aps').inputs.push('missing.v1'),
    ],
    [
      /provider must precede consumer/,
      (candidate) =>
        candidate.release.artifacts.find(({ id }) => id === 'render_runtime').inputs.push('gpt.v1'),
    ],
    [
      /deferred integration cannot provide/,
      (candidate) =>
        candidate.release.artifacts.find(({ id }) => id === 'gpt_later').outputs.push('late.v1'),
    ],
    [
      /bootstrap invariant/,
      (candidate) => candidate.release.artifacts[0].outputs.push('bootstrap.v1'),
    ],
    [
      /core invariant/,
      (candidate) =>
        candidate.release.artifacts.find(({ id }) => id === 'core').inputs.push('runtime.v1'),
    ],
  ];

  for (const [pattern, mutate] of mutations) {
    const candidate = readBuildEvidence();
    mutate(candidate);
    assert.throws(() => bundleBudgets.validateRoleCorrectTransfer(candidate), pattern);
  }
});

test('source ownership drift is report-only for an otherwise valid current graph', () => {
  const evidence = readBuildEvidence();
  const currentArtifactContents = new Map(
    evidence.release.artifacts.map(({ file }) => [
      file,
      fs.readFileSync(path.resolve(libDirectory, '../dist', file)),
    ])
  );
  const omittedModuleSource = structuredClone(evidence);
  const creativeIndex = omittedModuleSource.release.artifacts
    .filter(({ role }) => role !== 'bootstrap')
    .findIndex(({ id }) => id === 'creative');
  const creativeModule = omittedModuleSource.metrics.modules[creativeIndex];
  const sourceIndex = creativeModule.sources.findIndex(
    ({ file }) => file === 'src/shared/scheduler.ts'
  );
  assert.notEqual(sourceIndex, -1);
  assert.notEqual(creativeModule.sources[sourceIndex].file, creativeModule.entry);
  creativeModule.sources.splice(sourceIndex, 1);
  assert.doesNotThrow(() =>
    bundleBudgets.validateRoleCorrectTransfer({
      ...omittedModuleSource,
      currentArtifactContents,
    })
  );
});

test('harmless current source reassignment does not consult captured ownership', () => {
  const evidence = readBuildEvidence();
  const creative = evidence.metrics.modules.find(({ file }) => file === 'tsjs-creative.js');
  const datadome = evidence.metrics.modules.find(({ file }) => file === 'tsjs-datadome.js');
  const sourceIndex = creative.sources.findIndex(({ file }) => file === 'src/shared/async.ts');
  assert.notEqual(sourceIndex, -1);
  const [asyncSource] = creative.sources.splice(sourceIndex, 1);
  datadome.sources.push(asyncSource);

  assert.doesNotThrow(() => bundleBudgets.validateRoleCorrectTransfer(evidence));
});

test('current source classification fails closed for renamed provider source', () => {
  const { metrics, release } = readBuildEvidence();
  const gpt = metrics.modules.find(({ file }) => file === 'tsjs-gpt.js');
  const provider = gpt.sources.find(({ file }) => file === 'src/integrations/gpt/module.ts');
  provider.file = 'src/integrations/gpt/provider.ts';

  assert.match(
    bundleBudgets.findProductionGraphViolations(metrics, release).join('\n'),
    /current provider source is missing.*gpt\/module\.ts|unclassified current production source.*gpt\/provider\.ts/
  );
});

test('unknown shared source cannot bridge deferred and takeover artifacts', () => {
  const { metrics, release } = readBuildEvidence();
  const source = {
    file: 'src/shared/new_deferred_runtime.ts',
    renderedBytes: 1,
  };
  metrics.modules.find(({ file }) => file === 'tsjs-core.js').sources.push({ ...source });
  metrics.modules.find(({ file }) => file === 'tsjs-gpt_later.js').sources.push({ ...source });

  assert.match(
    bundleBudgets.findProductionGraphViolations(metrics, release).join('\n'),
    /unclassified current production source src\/shared\/new_deferred_runtime\.ts/
  );
});

test('provider implementation moved under core remains unclassified', () => {
  const { metrics, release } = readBuildEvidence();
  const gpt = metrics.modules.find(({ file }) => file === 'tsjs-gpt.js');
  const provider = gpt.sources.find(({ file }) => file === 'src/integrations/gpt/module.ts');
  provider.file = 'src/core/gpt_provider.ts';

  assert.match(
    bundleBudgets.findProductionGraphViolations(metrics, release).join('\n'),
    /unclassified current production source src\/core\/gpt_provider\.ts/
  );
});

test('deferred implementation moved under shared remains unclassified', () => {
  const { metrics, release } = readBuildEvidence();
  const prebidLater = metrics.modules.find(({ file }) => file === 'tsjs-prebid_later.js');
  const deferred = prebidLater.sources.find(
    ({ file }) => file === 'src/integrations/prebid/refresh.ts'
  );
  deferred.file = 'src/shared/prebid_refresh_runtime.ts';

  assert.match(
    bundleBudgets.findProductionGraphViolations(metrics, release).join('\n'),
    /unclassified current production source src\/shared\/prebid_refresh_runtime\.ts/
  );
});

test('new deferred-owned source cannot reach a takeover artifact', () => {
  const { metrics, release } = readBuildEvidence();
  const deferredSource = {
    file: 'src/integrations/gpt_diagnostics/presentation/new_panel.ts',
    renderedBytes: 1,
  };
  metrics.modules
    .find(({ file }) => file === 'tsjs-diagnostics_presentation.js')
    .sources.push({
      ...deferredSource,
    });
  metrics.modules.find(({ file }) => file === 'tsjs-core.js').sources.push({ ...deferredSource });

  assert.match(
    bundleBudgets.findProductionGraphViolations(metrics, release).join('\n'),
    /core reaches deferred-owned source.*new_panel\.ts/
  );
});

test('current graph inventory rejects cleared bootstrap sources', () => {
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
    /does not contain its entry source/
  );
});

test('current graph validation has no captured ownership-policy parameter', () => {
  const { metrics, release } = readBuildEvidence();
  assert.equal(bundleBudgets.findProductionGraphViolations.length, 2);
  assert.deepEqual(bundleBudgets.findProductionGraphViolations(metrics, release), []);
});

test('source ownership graph rejects duplicate bootstrap sources', () => {
  const { metrics, release } = readBuildEvidence();
  const duplicateBootstrapSource = structuredClone(metrics);
  duplicateBootstrapSource.bootstrap.sources.push(
    structuredClone(duplicateBootstrapSource.bootstrap.sources[0])
  );
  assert.throws(
    () => bundleBudgets.findProductionGraphViolations(duplicateBootstrapSource, release),
    /tsjs-bootstrap\.js\.sources\[.*\] is invalid/
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

test('transfer capture reports deltas without an acceptance ceiling', () => {
  assert.equal(typeof bundleBudgets.buildTransferCaptureReport, 'function');
  const captured = Object.fromEntries(
    ['bootstrap', 'minimal', 'reference', 'maximal'].map((setName) => [
      setName,
      { rawBytes: 10, gzipBytes: 10, brotliBytes: 10 },
    ])
  );
  const current = structuredClone(captured);
  for (const set of Object.values(current)) {
    set.rawBytes = 12;
    set.gzipBytes = 12;
    set.brotliBytes = 12;
  }
  const report = bundleBudgets.buildTransferCaptureReport(captured, current);
  assert.deepEqual(report.reference.gzipBytes, {
    capturedBytes: 10,
    currentBytes: 12,
    deltaBytes: 2,
  });
  assert.equal(Object.hasOwn(report.reference.gzipBytes, 'ceilingBytes'), false);
});

test('production bundle graphs reject every current forbidden edge', () => {
  const { metrics, release } = readBuildEvidence();
  assert.equal(typeof bundleBudgets.findProductionGraphViolations, 'function');
  assert.deepEqual(bundleBudgets.findProductionGraphViolations(metrics, release), []);
  const rejectSource = (artifactId, file, pattern) => {
    const candidate = structuredClone(metrics);
    const artifactIndex = release.artifacts
      .filter(({ role }) => role !== 'bootstrap')
      .findIndex(({ id }) => id === artifactId);
    candidate.modules[artifactIndex].sources.push({ file, renderedBytes: 1 });
    assert.match(
      bundleBudgets.findProductionGraphViolations(candidate, release).join('\n'),
      pattern
    );
  };

  rejectSource('gpt', 'src/integrations/render_runtime/module.ts', /inlines provider core/);
  rejectSource('aps', 'src/kernel/runtime.ts', /inlines provider core/);
  rejectSource('gpt', 'src/adapters/prebid.ts', /inlines provider prebid/);
  rejectSource('aps', 'src/shared/dom_insertion_dispatcher.ts', /forbidden shared source/);
  for (const artifactId of ['first_display', 'aps']) {
    rejectSource(
      artifactId,
      'src/shared/aps_documents.ts',
      /unclassified current production source/
    );
    rejectSource(
      artifactId,
      'src/core/contracts/generated/renderer_validator_document_v1.ts',
      /unclassified current production source/
    );
  }
  rejectSource('aps', 'src/test/fake_adapter.ts', /test\/fake\/no-op seam/);
  const vendoredProvider = structuredClone(metrics);
  vendoredProvider.modules[1].sources.push({
    file: 'node_modules/prebid.js/build/dist/prebid.js',
    renderedBytes: 1,
  });
  assert.throws(
    () => bundleBudgets.findProductionGraphViolations(vendoredProvider, release),
    /sources\[.*\] is invalid/
  );
});

test('production bundle graphs scan bootstrap sources for test and fake seams', () => {
  const { metrics, release } = readBuildEvidence();
  metrics.bootstrap.sources.push({ file: 'src/test/fake_adapter.ts', renderedBytes: 1 });

  assert.match(
    bundleBudgets.findProductionGraphViolations(metrics, release).join('\n'),
    /bootstrap reaches production test\/fake\/no-op seam src\/test\/fake_adapter\.ts/
  );
});

test('production bundle graphs reject provider implementation modules, not only entries', () => {
  const { metrics, release } = readBuildEvidence();
  const gptIndex = release.artifacts
    .filter(({ role }) => role !== 'bootstrap')
    .findIndex(({ id }) => id === 'gpt');
  metrics.modules[gptIndex].sources.push({
    file: 'src/integrations/render_runtime/module.ts',
    renderedBytes: 1,
  });

  assert.match(
    bundleBudgets.findProductionGraphViolations(metrics, release).join('\n'),
    /gpt inlines provider core.*src\/integrations\/render_runtime\/module\.ts/
  );
});

test('current provider policy rejects duplicated provider source without capture input', () => {
  const { metrics, release } = readBuildEvidence();
  const providerSource = 'src/services/render.ts';
  const gptIndex = release.artifacts
    .filter(({ role }) => role !== 'bootstrap')
    .findIndex(({ id }) => id === 'gpt');
  metrics.modules[gptIndex].sources.push({ file: providerSource, renderedBytes: 1 });

  assert.match(
    bundleBudgets.findProductionGraphViolations(metrics, release).join('\n'),
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
    const { metrics, release } = readBuildEvidence();
    const artifactIndex = release.artifacts
      .filter(({ role }) => role !== 'bootstrap')
      .findIndex(({ id }) => id === consumerId);
    metrics.modules[artifactIndex].sources.push({ file: providerSource, renderedBytes: 1 });

    assert.match(
      bundleBudgets.findProductionGraphViolations(metrics, release).join('\n'),
      new RegExp(
        `${consumerId} inlines provider ${providerId}.*${providerSource.replaceAll('.', '\\.')}`
      )
    );
  });
}

test('takeover bundle graph rejects deferred-presentation-only source ownership', () => {
  const { metrics, release } = readBuildEvidence();
  const coreIndex = release.artifacts
    .filter(({ role }) => role !== 'bootstrap')
    .findIndex(({ id }) => id === 'core');
  metrics.modules[coreIndex].sources.push({
    file: 'src/integrations/gpt_diagnostics/exhaustive.ts',
    renderedBytes: 1,
  });

  assert.match(
    bundleBudgets.findProductionGraphViolations(metrics, release).join('\n'),
    /core reaches deferred-owned source src\/integrations\/gpt_diagnostics\/exhaustive\.ts/
  );
});

test('production bundle graphs reject actual underscore-named test seams', () => {
  const { metrics, release } = readBuildEvidence();
  const apsIndex = release.artifacts
    .filter(({ role }) => role !== 'bootstrap')
    .findIndex(({ id }) => id === 'aps');
  metrics.modules[apsIndex].sources.push({
    file: 'src/composition/browser_test.ts',
    renderedBytes: 1,
  });

  assert.match(
    bundleBudgets.findProductionGraphViolations(metrics, release).join('\n'),
    /aps reaches production test\/fake\/no-op seam src\/composition\/browser_test\.ts/
  );
});

test('bundle check authenticates frozen captures and reports both without enforcing them', () => {
  const result = checkBundleBudgets();

  assert.equal(result.roleCorrectStatus, 'immutable-intermediate');
  assert.equal(result.reviewRemediationStatus, 'immutable-report-only');
  assert.equal(result.transferCapturesEnforced, false);
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
  assert.deepEqual(Object.keys(result.frozenTransferReports), [
    'roleCorrectTransfer',
    'reviewRemediationTransfer',
  ]);
  for (const captureReport of Object.values(result.frozenTransferReports)) {
    for (const report of Object.values(captureReport)) {
      for (const size of ['rawBytes', 'gzipBytes', 'brotliBytes']) {
        assert.equal(
          report[size].deltaBytes,
          report[size].currentBytes - report[size].capturedBytes
        );
        assert.equal(Object.hasOwn(report[size], 'ceilingBytes'), false);
      }
    }
  }

  const commandReport = bundleBudgets.summarizeBundleBudgetCommandReport(result);
  assert.equal(commandReport.candidateArchitecture.firstDisplay.reachableMaskCount, 2_560);
  assert.equal(
    commandReport.candidateArchitecture.firstDisplay.permittedMaskCount,
    result.candidateArchitecture.firstDisplay.permittedMasks.length
  );
  assert.equal(Object.hasOwn(commandReport.candidateArchitecture.firstDisplay, 'masks'), false);
});

test('takeover render trace source is data-only and guarded against presentation regression', () => {
  const traceSource = fs.readFileSync(path.join(libDirectory, 'src/core/trace.ts'), 'utf8');
  const architectureSource = fs.readFileSync(
    path.join(libDirectory, 'scripts/check-hard-cutover-absence.mjs'),
    'utf8'
  );

  assert.doesNotMatch(
    traceSource,
    /\b(?:Document|HTMLElement|MutationObserver)\b|createElement|getElementById|querySelector|clipboard|data-ts-/
  );
  assert.match(architectureSource, /core render trace presentation leakage/);
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

test('Vitest bounds worker concurrency with the supported Vitest 4 option', () => {
  const configSource = fs.readFileSync(path.join(libDirectory, 'vitest.config.ts'), 'utf8');

  assert.doesNotMatch(configSource, /\bthreads\s*:\s*false\b/u);
  assert.match(configSource, /\bmaxWorkers\s*:\s*2\b/u);
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

test('hard-cutover policy rejects every retired wire, runtime, and public surface', () => {
  const retired = [
    ['src/legacy.ts', '"/integrations/aps/renderer"'],
    [
      'src/legacy.ts',
      "JSON.stringify({message:'Prebid Response',rendererVersion:'4',rendererUrl})",
    ],
    ['src/legacy.ts', "port.postMessage({message:'TS APS Start'})"],
    ['src/legacy.ts', 'window.__tsjs_gpt_enabled = true'],
    ['src/legacy.ts', 'script.setAttribute("data-ts-gam-attribution", "1")'],
    ['src/legacy.ts', 'interface TsjsApiV1 {}'],
    ['src/legacy.ts', 'tsjs.renderAdUnit("slot")'],
    ['src/legacy.ts', 'tsjs.setConfig({debug: true})'],
    ['src/legacy.ts', 'const value = tsjs.getConfig()'],
    ['src/legacy.ts', 'const slots = tsjs.adSlots'],
    ['src/legacy.ts', 'const trace = tsjs.renders'],
    ['src/legacy.ts', 'const diagnostics = tsjs.gptDiagnostics'],
    ['src/legacy.ts', 'tsjs.version = "0.1.0"'],
    ['src/legacy.ts', 'window.dispatchEvent(new Event("tsjs:adRendered"))'],
    ['src/composition/browser.ts', 'export function createBrowserRuntime() {}'],
  ];
  for (const [file, source] of retired) {
    assert.ok(
      findCutoverTextViolations(file, source).length > 0,
      `retired cutover surface should be rejected: ${source}`
    );
  }
});

test('browser cutover fixtures do not reconstruct the retired integration config carrier', () => {
  for (const relativePath of [
    'crates/trusted-server-integration-tests/browser/tests/shared/aps-puc-lifecycle.spec.ts',
    'crates/trusted-server-integration-tests/browser/tests/shared/creative-sandbox.spec.ts',
    'crates/trusted-server-integration-tests/browser/tests/shared/tsjs-runtime.spec.ts',
  ]) {
    const source = fs.readFileSync(path.join(repositoryRoot, relativePath), 'utf8');
    assert.doesNotMatch(source, /_integrationConfig/u, relativePath);
  }
});

test('vendor boundary rejects APS, GPT, and PUC artifacts but permits conformance metadata', () => {
  const vendored = [
    ['fixtures/prebid-creative.js', 'window._aps = new Map();'],
    ['fixtures/gpt.js', 'window.googletag = window.googletag || {};'],
    ['fixtures/gpt.js.map', '{"sources":["gpt.js"]}'],
    ['fixtures/gpt.js.sha256', 'deadbeef'],
    ['fixtures/prebid-universal-creative-1.17.2.js', 'window.renderAd = function() {};'],
    ['fixtures/renamed.js', '/*! Prebid Universal Creative v1.17.2 */'],
    ['fixtures/renamed.js', '/*! @license Google Publisher Tag */'],
    ['fixtures/renamed.js', '/*! Copyright Amazon Publisher Services */'],
    ['fixtures/vendor.json', '{"pucIntegrity":"sha384-deadbeef"}'],
  ];
  for (const [file, source] of vendored) {
    assert.ok(
      findVendorBoundaryViolations(file, source).length > 0,
      `vendored upstream artifact should be rejected: ${file}`
    );
  }
  assert.deepEqual(
    findVendorBoundaryViolations(
      'browser/helpers/gam-test-network.ts',
      'export const REAL_GAM_PUC_RELEASE = "1.17.2"; Object.freeze({pucRelease: REAL_GAM_PUC_RELEASE});'
    ),
    []
  );
  assert.deepEqual(
    findVendorBoundaryViolations(
      'browser/fixtures/fictional-aps-runner.js',
      '// Fictional hermetic APS runner fixture; not copied from APS.\nwindow._aps = new Map();'
    ),
    []
  );
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

function workflowJob(source, name) {
  const marker = `  ${name}:\n`;
  const start = source.indexOf(marker);
  assert.notEqual(start, -1, `workflow must contain ${name}`);
  const remainder = source.slice(start + marker.length);
  const nextJob = remainder.search(/^ {2}[a-zA-Z0-9_-]+:\n/mu);
  return nextJob === -1 ? remainder : remainder.slice(0, nextJob);
}

test('APS and TSJS workflows keep feature programs in repository script files', () => {
  const workflows = {
    performance: fs.readFileSync(
      path.join(repositoryRoot, '.github/workflows/tsjs-performance-gate.yml'),
      'utf8'
    ),
    realGam: fs.readFileSync(
      path.join(repositoryRoot, '.github/workflows/aps-real-gam.yml'),
      'utf8'
    ),
    quality: workflowJob(
      fs.readFileSync(path.join(repositoryRoot, '.github/workflows/test.yml'), 'utf8'),
      'cutover-quality-evidence'
    ),
    conformance: workflowJob(
      fs.readFileSync(path.join(repositoryRoot, '.github/workflows/integration-tests.yml'), 'utf8'),
      'browser-tests-aps-tsjs-conformance'
    ),
    cutover: workflowJob(
      fs.readFileSync(path.join(repositoryRoot, '.github/workflows/integration-tests.yml'), 'utf8'),
      'cutover-suite'
    ),
  };
  const combined = Object.values(workflows).join('\n');

  for (const [name, workflow] of Object.entries(workflows)) {
    assert.doesNotMatch(workflow, /^\s*run:\s*[>|]/mu, `${name} must not embed a run program`);
    assert.doesNotMatch(workflow, /\bnode\s+-e\b/u, `${name} must not embed JavaScript`);
    assert.doesNotMatch(
      workflow,
      /(?:^|\n)\s*(?:for|while)\s+\S+/u,
      `${name} must not embed loops`
    );
    assert.doesNotMatch(
      workflow,
      /<<-?['"]?[A-Z][A-Z0-9_]*['"]?/u,
      `${name} must not embed heredocs`
    );
  }

  for (const script of [
    'scripts/ci/read-toolchains.sh',
    'scripts/ci/aps-real-gam.sh',
    'scripts/ci/aps-tsjs-cutover.sh',
    'scripts/ci/aps-tsjs-evidence.mjs',
    'scripts/ci/aps-tsjs-quality.sh',
    'scripts/ci/tsjs-performance.sh',
  ]) {
    assert.ok(combined.includes(script), `a workflow must invoke ${script}`);
    assert.ok(fs.existsSync(path.join(repositoryRoot, script)), `${script} must be a real file`);
  }

  for (const match of combined.matchAll(/(?:bash|node)\s+(scripts\/ci\/[^\s"']+)/gu)) {
    assert.ok(
      fs.existsSync(path.join(repositoryRoot, match[1])),
      `workflow script target must exist: ${match[1]}`
    );
  }

  const performanceScript = fs.readFileSync(
    path.join(repositoryRoot, 'scripts/ci/tsjs-performance.sh'),
    'utf8'
  );
  const performanceConfig =
    'crates/trusted-server-integration-tests/browser/playwright.performance.config.ts';
  assert.doesNotMatch(
    performanceScript,
    /printf[\s\S]*export default/u,
    'the performance action must not synthesize an executable config'
  );
  assert.ok(
    performanceScript.includes(`--config="$repository_root/${performanceConfig}"`),
    'the performance action must invoke its checked-in Playwright config'
  );
  assert.ok(
    fs.existsSync(path.join(repositoryRoot, performanceConfig)),
    'the performance Playwright config must be a real repository file'
  );
  const performanceConfigSource = fs.readFileSync(
    path.join(repositoryRoot, performanceConfig),
    'utf8'
  );
  assert.match(
    performanceConfigSource,
    /timeout: 30_000[\s\S]*retries: 0[\s\S]*workers: 1[\s\S]*browserName: "chromium"/u,
    'the checked-in performance config must preserve the immutable single-Chromium instrument'
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
  const qualityScript = fs.readFileSync(
    path.join(repositoryRoot, 'scripts/ci/aps-tsjs-quality.sh'),
    'utf8'
  );
  const cutoverScript = fs.readFileSync(
    path.join(repositoryRoot, 'scripts/ci/aps-tsjs-cutover.sh'),
    'utf8'
  );
  const evidenceScript = fs.readFileSync(
    path.join(repositoryRoot, 'scripts/ci/aps-tsjs-evidence.mjs'),
    'utf8'
  );
  const evidenceSelfTest = execFileSync(
    process.execPath,
    [path.join(repositoryRoot, 'scripts/ci/aps-tsjs-evidence.mjs'), 'self-test'],
    { encoding: 'utf8' }
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
    assert.match(workflow, /scripts\/ci\/aps-tsjs-evidence\.mjs/, `${name} evidence script`);
  }
  for (const [name, workflow] of [
    ['integration', integrationWorkflow],
    ['real-GAM', realGamWorkflow],
  ]) {
    assert.match(workflow, /previous_artifact_id:[\s\S]*?required: true/, `${name} prior input`);
  }
  assert.match(evidenceScript, /evidence-manifest\.json/);
  assert.match(evidenceScript, /commitSha/);
  assert.match(evidenceScript, /runId/);
  assert.match(evidenceScript, /conclusion/);
  assert.match(evidenceScript, /previousArtifactId/);
  assert.match(evidenceSelfTest, /APS\/TSJS evidence self-test passed/);
  assert.match(qualityWorkflow, /aps-tsjs-quality-\$\{\{ github\.run_id \}\}/);
  assert.match(qualityScript, /set -euo pipefail[\s\S]*?quality\.log/);
  assert.match(qualityScript, /tsjs-build-metrics-v1\.json/);
  assert.match(integrationWorkflow, /aps-tsjs-cutover-\$\{\{ github\.sha \}\}/);
  assert.match(cutoverScript, /for runtime in axum fastly cloudflare spin/);
  assert.match(cutoverScript, /aps-proxy-\$runtime\.log/);
  assert.match(cutoverScript, /--project=chromium --project=firefox --project=webkit/);
  assert.match(
    cutoverScript,
    /TS_BROWSER_PROJECTS=chromium,firefox,webkit[\s\\]*npx playwright test/,
    'the cutover script must declare every requested Playwright project itself'
  );
  assert.match(integrationWorkflow, /Scrub all integration evidence before upload/);
  assert.match(realGamWorkflow, /aps-real-gam-\$\{\{ github\.run_id \}\}/);
  assert.match(evidenceScript, /capabilities\?/);
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
      bundle('core', 'a', 'integration', 'takeover', 'first_display_or_idle'),
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
