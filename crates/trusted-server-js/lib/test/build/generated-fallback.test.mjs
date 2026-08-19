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
const selectedSrc = `/static/tsjs=tsjs-first-display.min.js?m=0001&v=${'c'.repeat(64)}`;
const runtimeSrc = `/static/tsjs=tsjs-unified.min.js?v=${'d'.repeat(64)}`;
const EMPTY_INTEGRATION_CONFIGS = Object.freeze({ version: 1, entries: Object.freeze([]) });
const EMPTY_INTEGRATION_CONFIG_DIGEST = createHash('sha256')
  .update(JSON.stringify(EMPTY_INTEGRATION_CONFIGS))
  .digest('hex');

function createDocument() {
  const dom = new JSDOM(
    `<!doctype html><head></head><body><script id="trustedserver-js" src="${selectedSrc}"></script></body>`,
    { runScripts: 'outside-only', url: 'https://publisher.example/page' }
  );
  dom.window.TextEncoder = TextEncoder;
  dom.window.TextDecoder = TextDecoder;
  Object.defineProperty(dom.window.performance, 'mark', {
    configurable: true,
    value: () => undefined,
  });
  const selected = dom.window.document.querySelector('script#trustedserver-js');
  Object.defineProperty(dom.window.document, 'currentScript', {
    configurable: true,
    value: selected,
  });
  return { dom, selected };
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

function evaluateWithInput(dom) {
  dom.window.eval(
    `const __TSJS_SERVER_BOOT_INPUT_V1__={target:window.tsjs,boot:${JSON.stringify(
      boot()
    )},outline:${JSON.stringify(outline())}};${source}`
  );
}

test('generated bootstrap bytes are stamped exactly once and expose no callable global', () => {
  assert.equal(source.includes('__TSJS_RELEASE_ID_SENTINEL_V1__'), false);
  assert.equal(source.split(manifest.releaseId).length - 1, 1);
  const { dom } = createDocument();
  dom.window.tsjs = {};

  evaluateWithInput(dom);

  assert.equal(dom.window.tsjs_bootstrap, undefined);
  assert.equal(dom.window.tsjs.boot.releaseId, manifest.releaseId);
  assert.equal(typeof dom.window.tsjs._registerFirstDisplay, 'function');
  dom.window.close();
});

test('generated bootstrap leaves the namespace untouched without exact server input', () => {
  for (const input of ['', 'const __TSJS_SERVER_BOOT_INPUT_V1__={};']) {
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

  dom.window.eval(
    `const __TSJS_SERVER_BOOT_INPUT_V1__={target:window.tsjs,boot:${JSON.stringify(
      directBoot
    )},outline:null};${source}`
  );

  const cancel = () => assert.fail('committed direct runtime must not be cancelled');
  const complete = target._claimDirectRuntime(selected, cancel);
  assert.equal(typeof complete, 'function');
  complete('kernel');
  assert.equal(Object.hasOwn(target, '_claimDirectRuntime'), false);
  assert.equal(Object.hasOwn(target, '_internal'), false);
  assert.equal(target.boot.manifest.firstDisplay, null);
  dom.window.close();
});

test('generated bootstrap commits one non-rendering terminal shell after registration failure', async () => {
  const { dom, selected } = createDocument();
  const drained = [];
  const target = {
    que: [
      function () {
        drained.push(this);
      },
    ],
  };
  dom.window.tsjs = target;

  evaluateWithInput(dom);
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
  assert.throws(() => target.addAdUnits({}), {
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
  dom.window.close();
});

test('generated bootstrap does not overwrite a conflicting non-configurable namespace field', () => {
  const { dom, selected } = createDocument();
  const target = { que: [] };
  const publisherApi = () => undefined;
  Object.defineProperty(target, 'publisherApi', {
    configurable: false,
    enumerable: true,
    value: publisherApi,
    writable: false,
  });
  dom.window.tsjs = target;

  evaluateWithInput(dom);
  assert.equal(dom.window.tsjs._registerFirstDisplay.call(target, {}, selected), false);

  assert.equal(target.publisherApi, publisherApi);
  assert.equal(Object.hasOwn(target, 'releaseId'), false);
  assert.equal(Object.hasOwn(target, '_internal'), false);
  dom.window.close();
});
