import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { TextDecoder, TextEncoder } from 'node:util';

import { JSDOM } from 'jsdom';

const dist = path.resolve(import.meta.dirname, '../../../dist');
const manifest = JSON.parse(readFileSync(path.join(dist, 'tsjs-release-v1.json'), 'utf8'));
const source = readFileSync(path.join(dist, 'tsjs-bootstrap.js'), 'utf8');
const artifactById = new Map(manifest.artifacts.map((artifact) => [artifact.id, artifact]));
const selectedSrc = `/static/tsjs=tsjs-first-display.min.js?m=0001&v=${'c'.repeat(64)}`;
const selectedGptSrc = `/static/tsjs=tsjs-first-display.min.js?m=0081&v=${'c'.repeat(64)}`;
const runtimeSrc = `/static/tsjs=tsjs-unified.min.js?v=${'d'.repeat(64)}`;
const EMPTY_INTEGRATION_CONFIGS = Object.freeze({ version: 1, entries: Object.freeze([]) });
const EMPTY_INTEGRATION_CONFIG_DIGEST = createHash('sha256')
  .update(JSON.stringify(EMPTY_INTEGRATION_CONFIGS))
  .digest('hex');
const plain = (value) => JSON.parse(JSON.stringify(value));

function createDocument(firstDisplaySrc = selectedSrc) {
  const dom = new JSDOM(
    `<!doctype html><head></head><body><script id="trustedserver-js" src="${firstDisplaySrc}"></script></body>`,
    { runScripts: 'outside-only', url: 'https://publisher.example/page' }
  );
  dom.window.TextEncoder = TextEncoder;
  dom.window.TextDecoder = TextDecoder;
  Object.defineProperty(dom.window.performance, 'mark', {
    configurable: true,
    value: () => undefined,
  });
  const animationFrames = [];
  Object.defineProperty(dom.window, 'requestAnimationFrame', {
    configurable: true,
    value: (callback) => {
      animationFrames.push(callback);
      return animationFrames.length;
    },
  });
  const selected = dom.window.document.querySelector('script#trustedserver-js');
  Object.defineProperty(dom.window.document, 'currentScript', {
    configurable: true,
    value: selected,
  });
  return { animationFrames, dom, selected };
}

function boot() {
  return {
    abi: 1,
    releaseId: manifest.releaseId,
    manifest: {
      version: 1,
      releaseId: manifest.releaseId,
      firstDisplay: { src: selectedSrc, slices: ['first_display'] },
      runtimeSrc,
      integrations: [],
    },
    auctionProjection: {
      version: 1,
      auction: {
        version: 1,
        auctionId: 'initial',
        results: [{ slot: 'slot-1', outcome: 'no_bid' }],
      },
      slots: [
        {
          slot: 'slot-1',
          gamUnitPath: '/123/slot-1',
          divId: 'slot-1',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: [],
    },
    integrations: EMPTY_INTEGRATION_CONFIGS,
    creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
    diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
  };
}

function outline() {
  return {
    version: 1,
    releaseId: manifest.releaseId,
    generation: 1,
    projectionDigest: 'e'.repeat(64),
    integrationConfigDigest: EMPTY_INTEGRATION_CONFIG_DIGEST,
    slices: ['first_display'],
    slotCount: 1,
    outcomeCount: 1,
    capabilities: [],
    objectKinds: [],
  };
}

function selectGpt(bootValue, outlineValue) {
  bootValue.manifest.firstDisplay = {
    src: selectedGptSrc,
    slices: ['first_display', 'gpt_initial'],
  };
  outlineValue.slices = ['first_display', 'gpt_initial'];
}

function selectInitialSlice(bootValue, outlineValue, id, config) {
  const firstDisplay = manifest.artifacts.filter((artifact) =>
    ['first_display_base', 'first_display_slice'].includes(artifact.role)
  );
  const index = firstDisplay.findIndex((artifact) => artifact.id === id);
  assert.ok(index > 0, `missing first-display slice ${id}`);
  const mask = (1 | (1 << index)).toString(16).padStart(4, '0');
  const artifact = artifactById.get(id);
  const body = readFileSync(path.join(dist, artifact.file), 'utf8');
  const hash = createHash('sha256').update(body).digest('hex');
  const src = `/static/tsjs=tsjs-first-display.min.js?m=${mask}&v=${hash}`;
  bootValue.manifest.firstDisplay = { src, slices: ['first_display', id] };
  const product = id.slice(0, -'_initial'.length);
  bootValue.integrations = { version: 1, entries: [{ id: product, config }] };
  outlineValue.slices = ['first_display', id];
  return { body, src };
}

function transport(bootValue = boot(), outlineValue = outline()) {
  const integrity = {
    version: 1,
    projectionDigest: createHash('sha256')
      .update(JSON.stringify(bootValue.auctionProjection))
      .digest('hex'),
    integrationConfigDigest: createHash('sha256')
      .update(JSON.stringify(bootValue.integrations))
      .digest('hex'),
  };
  if (outlineValue) {
    outlineValue.projectionDigest = integrity.projectionDigest;
    outlineValue.integrationConfigDigest = integrity.integrationConfigDigest;
  }
  return { version: 1, boot: bootValue, integrity, outline: outlineValue };
}

function evaluateTransport(dom, value) {
  dom.window.eval(
    `const __TSJS_SERVER_BOOT_TRANSPORT_V1__=${JSON.stringify(JSON.stringify(value))};${source}`
  );
}

function evaluateWithInput(dom) {
  evaluateTransport(dom, transport());
}

test('generated bootstrap bytes are stamped exactly once and expose no callable global', () => {
  assert.equal(source.includes('__TSJS_RELEASE_ID_SENTINEL_V1__'), false);
  assert.equal(source.split(manifest.releaseId).length - 1, 1);
  const { dom } = createDocument();
  dom.window.tsjs = {};

  evaluateWithInput(dom);

  assert.equal(dom.window.tsjs_bootstrap, undefined);
  assert.equal(dom.window.tsjs.boot.releaseId, manifest.releaseId);
  assert.equal(Object.hasOwn(dom.window.tsjs, '_registerFirstDisplay'), false);
  dom.window.close();
});

test('generated bootstrap leaves the namespace untouched without exact server input', () => {
  for (const input of ['', 'const __TSJS_SERVER_BOOT_TRANSPORT_V1__={};']) {
    const { dom } = createDocument();
    const target = { publisher: 'retained' };
    dom.window.tsjs = target;

    dom.window.eval(`${input}${source}`);

    assert.equal(dom.window.tsjs, target);
    assert.deepEqual(Object.keys(target), ['publisher']);
    dom.window.close();
  }
});

test('generated bootstrap transfers the direct persistent watchdog to the selected runtime', () => {
  const { dom, selected } = createDocument();
  selected.src = runtimeSrc;
  const target = { que: [] };
  dom.window.tsjs = target;
  const directBoot = boot();
  directBoot.manifest.firstDisplay = null;

  evaluateTransport(dom, transport(directBoot, null));

  const cancel = () => assert.fail('committed direct runtime must not be cancelled');
  const complete = target._claimDirectRuntime(selected, cancel);
  assert.equal(typeof complete, 'function');
  complete('kernel');
  assert.equal(Object.hasOwn(target, '_claimDirectRuntime'), false);
  assert.equal(Object.hasOwn(target, '_internal'), false);
  assert.equal(target.boot.manifest.firstDisplay, null);
  dom.window.close();
});

test('generated bootstrap classifies a rejected claimed boot as an ABI mismatch', () => {
  const { dom, selected } = createDocument();
  selected.src = runtimeSrc;
  const target = { que: [] };
  dom.window.tsjs = target;
  const directBoot = boot();
  directBoot.manifest.firstDisplay = null;

  evaluateTransport(dom, transport(directBoot, null));

  const claim = target._claimBootSnapshot;
  assert.equal(typeof claim, 'function');
  const claimed = claim(selected);
  assert.equal(claimed.boot, target.boot);
  claimed.complete('abi_mismatch');
  assert.deepEqual(
    JSON.parse(JSON.stringify(Object.getOwnPropertyDescriptor(target, '_internal')?.value)),
    {
      state: 'fallback',
      releaseId: manifest.releaseId,
      reason: 'abi_mismatch',
      initialDisplayCommitted: false,
    }
  );
  dom.window.close();
});

test('generated bootstrap reserves the boot claim across reentrant DOM authentication', () => {
  const { dom, selected } = createDocument();
  selected.src = runtimeSrc;
  const target = { que: [] };
  dom.window.tsjs = target;
  const directBoot = boot();
  directBoot.manifest.firstDisplay = null;

  evaluateTransport(dom, transport(directBoot, null));

  const claim = target._claimBootSnapshot;
  const querySelectorAll = dom.window.document.querySelectorAll;
  let nested;
  let reentered = false;
  Object.defineProperty(dom.window.document, 'querySelectorAll', {
    configurable: true,
    value(selector) {
      if (!reentered) {
        reentered = true;
        nested = claim(selected);
      }
      return Reflect.apply(querySelectorAll, this, [selector]);
    },
  });

  const claimed = claim(selected);
  nested?.complete('kernel');
  claimed.complete('abi_mismatch');

  assert.equal(nested, undefined);
  assert.deepEqual(
    JSON.parse(JSON.stringify(Object.getOwnPropertyDescriptor(target, '_internal')?.value)),
    {
      state: 'fallback',
      releaseId: manifest.releaseId,
      reason: 'abi_mismatch',
      initialDisplayCommitted: false,
    }
  );
  dom.window.close();
});

test('generated bootstrap completes a claimed boot without a reentrant realm callback', () => {
  const { dom, selected } = createDocument();
  selected.src = runtimeSrc;
  const target = { que: [] };
  dom.window.tsjs = target;
  const directBoot = boot();
  directBoot.manifest.firstDisplay = null;

  evaluateTransport(dom, transport(directBoot, null));

  const claimed = target._claimBootSnapshot(selected);
  const includes = dom.window.Array.prototype.includes;
  let reentered = false;
  Object.defineProperty(dom.window.Array.prototype, 'includes', {
    configurable: true,
    value(value, fromIndex) {
      if (!reentered) {
        reentered = true;
        claimed.complete('kernel');
      }
      return Reflect.apply(includes, this, [value, fromIndex]);
    },
    writable: true,
  });

  assert.doesNotThrow(() => claimed.complete('abi_mismatch'));
  assert.equal(reentered, false);
  assert.deepEqual(
    JSON.parse(JSON.stringify(Object.getOwnPropertyDescriptor(target, '_internal')?.value)),
    {
      state: 'fallback',
      releaseId: manifest.releaseId,
      reason: 'abi_mismatch',
      initialDisplayCommitted: false,
    }
  );
  dom.window.close();
});

test('generated base-only takeover failure leaves terminal fallback ownership with bootstrap', () => {
  const { animationFrames, dom } = createDocument();
  const target = { que: [] };
  dom.window.tsjs = target;
  evaluateWithInput(dom);

  const accepted = dom.window.document.createElement('iframe');
  accepted.id = 'accepted-first-display';
  dom.window.document.body.append(accepted);
  while (animationFrames.length > 0) animationFrames.shift()(dom.window.performance.now());
  const runtime = dom.window.document.querySelector('script#trustedserver-js-runtime');
  assert.ok(runtime, 'the bootstrap-owned base must reach protected paint without a marker');
  Object.defineProperty(dom.window.document, 'currentScript', {
    configurable: true,
    value: runtime,
  });
  const claimed = target._claimBootSnapshot(runtime);
  assert.equal(claimed.boot, target.boot);
  claimed.complete('bundle_partial');

  assert.deepEqual(
    JSON.parse(JSON.stringify(Object.getOwnPropertyDescriptor(target, '_internal')?.value)),
    {
      state: 'fallback',
      releaseId: manifest.releaseId,
      reason: 'bundle_partial',
      initialDisplayCommitted: false,
    }
  );
  assert.equal(target.boot.manifest.firstDisplay.src, selectedSrc);
  assert.equal(accepted.isConnected, true);
  dom.window.close();
});

test('generated bootstrap stops optional activation after a component installer fails', () => {
  const { dom, selected } = createDocument(selectedGptSrc);
  const target = { que: [] };
  const events = [];
  let bindingKeys;
  const bootValue = boot();
  const outlineValue = outline();
  selectGpt(bootValue, outlineValue);
  bootValue.integrations = { version: 1, entries: [{ id: 'gpt', config: {} }] };
  dom.window.tsjs = target;
  evaluateTransport(dom, transport(bootValue, outlineValue));

  const gpt = Object.freeze([
    1,
    'gpt_initial',
    manifest.releaseId,
    (bindings) => {
      bindingKeys = Reflect.ownKeys(bindings);
      events.push('install-gpt');
      throw new TypeError('invalid generated slice');
    },
  ]);

  assert.equal(target._registerFirstDisplay.call(target, gpt, selected), false);
  assert.deepEqual(bindingKeys, ['browser', 'observe', 'register']);
  assert.deepEqual(events, ['install-gpt']);
  assert.equal(Object.getOwnPropertyDescriptor(target, '_internal')?.value.reason, 'abi_mismatch');
  dom.window.close();
});

test('generated optional slices receive their exact parser-time browser bindings', () => {
  const fixtures = [
    ['datadome_initial', {}],
    ['didomi_initial', { proxyPath: '/integrations/didomi/consent/' }],
    ['google_tag_manager_initial', {}],
    ['lockr_initial', {}],
    ['osano_initial', {}],
    ['permutive_initial', {}],
    ['sourcepoint_initial', { rewriteSdk: true }],
    ['testlight_initial', {}],
  ];
  for (const [id, config] of fixtures) {
    const bootValue = boot();
    const outlineValue = outline();
    const { body, src } = selectInitialSlice(bootValue, outlineValue, id, config);
    const { dom } = createDocument(src);
    const callback = () => undefined;
    if (id === 'lockr_initial') dom.window.identityLockr = { host: 'vendor.example' };
    if (id === 'osano_initial') {
      dom.window.__uspapi = (_command, _version, complete) => complete({ uspString: '1YN-' }, true);
      dom.window.__gpp = (_command, complete) =>
        complete({ gppString: 'DBABLA~BVQqAAAAAgA.QA', applicableSections: [7] }, true);
      dom.window.__tcfapi = (_command, _version, complete) =>
        complete(
          {
            tcString:
              'COwK6gaOwK6gaFmAAAENAPCAAAAAAAAAAAAAAAAAAAAA.IFMsv_Z_G____bvQXQ1f9eY1f9_z_q7ff_3_3-_-3dV1v9zLv9____39nP___9v-_3_f__9P',
          },
          true
        );
    }
    if (id === 'permutive_initial') {
      dom.window.permutive = {
        config: {
          apiHost: 'api.vendor.example',
          apiProtocol: 'https',
          cdnBaseUrl: 'cdn.vendor.example',
          cdnProtocol: 'https',
          secureSignalsApiHost: 'signals.vendor.example',
          segmentSyncApiHost: 'sync.vendor.example',
        },
      };
      dom.window.localStorage.setItem(
        'permutive-app',
        JSON.stringify({ core: { cohorts: { all: ['one', 2] } } })
      );
    }
    if (id === 'sourcepoint_initial') {
      dom.window.localStorage.setItem(
        '_sp_user_consent_test',
        JSON.stringify({ gppData: { gppString: 'DBABLA', applicableSections: [7] } })
      );
    }
    if (id === 'testlight_initial') dom.window.testlight = { que: [callback] };
    dom.window.tsjs = { que: [] };

    evaluateTransport(dom, transport(bootValue, outlineValue));
    dom.window.eval(body);

    assert.equal(
      Object.getOwnPropertyDescriptor(dom.window.tsjs, '_internal'),
      undefined,
      `${id} must not force fallback`
    );
    if (id === 'datadome_initial' || id === 'google_tag_manager_initial') {
      const script = dom.window.document.createElement('script');
      script.src =
        id === 'datadome_initial'
          ? 'https://js.datadome.co/tags.js'
          : 'https://www.googletagmanager.com/gtm.js?id=GTM-TEST';
      dom.window.document.head.appendChild(script);
      assert.match(script.src, /\/integrations\/(?:datadome|google_tag_manager)\//);
    }
    if (id === 'didomi_initial') {
      assert.equal(
        dom.window.didomiConfig.sdkPath,
        'https://publisher.example/integrations/didomi/consent/'
      );
    }
    if (id === 'lockr_initial') {
      assert.equal(
        dom.window.identityLockr.host,
        'https://publisher.example/integrations/lockr/api'
      );
    }
    if (id === 'permutive_initial') {
      assert.equal(
        dom.window.permutive.config.apiHost,
        'publisher.example/integrations/permutive/api'
      );
    }
    if (id === 'sourcepoint_initial') assert.match(dom.window.document.cookie, /__gpp=DBABLA/);
    if (id === 'testlight_initial') assert.ok(dom.window.tsjs.que.includes(callback));
    dom.window.close();
  }
});

test('generated bootstrap commits one non-rendering terminal shell after registration failure', async () => {
  const { dom, selected } = createDocument(selectedGptSrc);
  const drained = [];
  const target = {
    que: [
      function () {
        drained.push(this);
      },
    ],
  };
  dom.window.tsjs = target;

  const bootValue = boot();
  const outlineValue = outline();
  selectGpt(bootValue, outlineValue);
  evaluateTransport(dom, transport(bootValue, outlineValue));
  assert.equal(dom.window.tsjs._registerFirstDisplay.call(target, {}, selected), false);

  assert.deepEqual(Object.keys(target).sort(), [
    '_registerIntegration',
    'addAdUnits',
    'boot',
    'log',
    'que',
    'releaseId',
    'requestAds',
    'version',
  ]);
  assert.deepEqual(
    JSON.parse(JSON.stringify(Object.getOwnPropertyDescriptor(target, '_internal')?.value)),
    {
      state: 'fallback',
      releaseId: manifest.releaseId,
      reason: 'abi_mismatch',
      initialDisplayCommitted: false,
    }
  );
  assert.equal(Object.getOwnPropertyDescriptor(target, '_internal')?.enumerable, false);
  assert.equal(target._registerIntegration(), false);
  assert.equal(drained.length, 1);
  assert.equal(drained[0], target);
  assert.equal(Object.isFrozen(target.que), true);
  assert.equal(
    target.que.push(() => drained.push('late')),
    0
  );
  assert.deepEqual(drained, [target, 'late']);
  const malformedUnit = dom.window.eval("({code:'',mediaTypes:{}})");
  assert.throws(() => target.addAdUnits(malformedUnit), {
    name: 'TsjsUnavailableError',
    code: 'runtime_unavailable',
    releaseId: manifest.releaseId,
    reason: 'abi_mismatch',
  });
  const validUnit = dom.window.eval(
    "({code:'programmatic',mediaTypes:{banner:{sizes:[[300,250]]}}})"
  );
  assert.throws(() => target.addAdUnits(validUnit), {
    name: 'TsjsUnavailableError',
    code: 'runtime_unavailable',
    releaseId: manifest.releaseId,
    reason: 'abi_mismatch',
  });
  await assert.rejects(target.requestAds(), {
    name: 'TsjsUnavailableError',
    code: 'runtime_unavailable',
    releaseId: manifest.releaseId,
    reason: 'abi_mismatch',
  });
  const explicitOptions = dom.window.eval("({slots:['slot-1']})");
  await assert.rejects(target.requestAds(explicitOptions), {
    name: 'TsjsUnavailableError',
    code: 'runtime_unavailable',
    releaseId: manifest.releaseId,
    reason: 'abi_mismatch',
  });
  const controller = new dom.window.AbortController();
  controller.abort();
  const abortedOptions = dom.window.eval("({slots:['slot-1']})");
  abortedOptions.signal = controller.signal;
  await assert.rejects(target.requestAds(abortedOptions), {
    name: 'TsjsUnavailableError',
    code: 'runtime_unavailable',
    releaseId: manifest.releaseId,
    reason: 'abi_mismatch',
  });
  assert.deepEqual(plain(target.boot.auctionProjection), {
    version: 1,
    auction: { version: 1, auctionId: 'fallback', results: [] },
    slots: [],
    bids: [],
  });
  assert.deepEqual(plain(target.boot.integrations), { version: 1, entries: [] });
  dom.window.close();
});

test('generated bootstrap does not overwrite a conflicting non-configurable namespace field', () => {
  const { dom, selected } = createDocument(selectedGptSrc);
  const target = { que: [] };
  const publisherApi = () => undefined;
  Object.defineProperty(target, 'publisherApi', {
    configurable: false,
    enumerable: true,
    value: publisherApi,
    writable: false,
  });
  dom.window.tsjs = target;

  const bootValue = boot();
  const outlineValue = outline();
  selectGpt(bootValue, outlineValue);
  evaluateTransport(dom, transport(bootValue, outlineValue));
  assert.equal(dom.window.tsjs._registerFirstDisplay.call(target, {}, selected), false);

  assert.equal(target.publisherApi, publisherApi);
  assert.equal(Object.hasOwn(target, 'releaseId'), false);
  assert.equal(Object.hasOwn(target, '_internal'), false);
  dom.window.close();
});
