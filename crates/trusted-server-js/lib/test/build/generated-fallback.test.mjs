import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { TextDecoder, TextEncoder } from 'node:util';

import { JSDOM } from 'jsdom';

const dist = path.resolve(import.meta.dirname, '../../../dist');
const manifest = JSON.parse(readFileSync(path.join(dist, 'tsjs-release-v1.json'), 'utf8'));
const source = readFileSync(path.join(dist, 'gpt-bootstrap-fallback.js'), 'utf8');

test('generated fallback bytes are stamped, executable, and add no callable global', async () => {
  assert.equal(source.includes('__TSJS_RELEASE_ID_SENTINEL_V1__'), false);
  assert.equal(source.split(manifest.releaseId).length - 1, 1);
  const dom = new JSDOM('<!doctype html>', { runScripts: 'outside-only' });
  dom.window.TextEncoder = TextEncoder;
  dom.window.TextDecoder = TextDecoder;
  const queued = [];
  dom.window.tsjs = {
    diagnostics: { legacy: true },
    adInit() {
      throw new Error('legacy runtime must be removed');
    },
    que: [
      function () {
        queued.push(this);
      },
    ],
    boot: {
      abi: 1,
      releaseId: 'b'.repeat(64),
      manifest: { version: 1, releaseId: 'b'.repeat(64), integrations: [] },
      auctionProjection: {
        version: 1,
        auction: { version: 1, auctionId: 'boot', results: [] },
        bids: [],
      },
      creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
      diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
    },
  };

  dom.window.eval(source);

  assert.equal(dom.window.tsjs.releaseId, manifest.releaseId);
  assert.equal(dom.window.tsjs.boot.releaseId, manifest.releaseId);
  assert.equal(dom.window.tsjs.boot.manifest.releaseId, manifest.releaseId);
  assert.equal(dom.window.tsjs._internal.reason, 'bundle_partial');
  assert.equal(Object.hasOwn(dom.window.tsjs, 'diagnostics'), false);
  assert.equal(Object.hasOwn(dom.window.tsjs, 'adInit'), false);
  assert.equal(queued.length, 1);
  assert.equal(queued[0], dom.window.tsjs);
  assert.equal(dom.window.tsjs_gpt_bootstrap_fallback, undefined);
  assert.equal(JSON.stringify(await dom.window.tsjs.requestAds()), '{"slots":[]}');
  dom.window.close();
});

test('generated fallback leaves a conflicting namespace queue untouched', () => {
  const dom = new JSDOM('<!doctype html>', { runScripts: 'outside-only' });
  dom.window.TextEncoder = TextEncoder;
  dom.window.TextDecoder = TextDecoder;
  const queued = () => undefined;
  const queue = [queued];
  const namespace = { que: queue, boot: {} };
  Object.defineProperty(namespace, 'adInit', {
    configurable: false,
    enumerable: true,
    value: () => undefined,
    writable: false,
  });
  dom.window.tsjs = namespace;

  dom.window.eval(source);

  assert.equal(dom.window.tsjs, namespace);
  assert.equal(dom.window.tsjs.que, queue);
  assert.equal(queue.length, 1);
  assert.equal(queue[0], queued);
  assert.equal(queue.push, Array.prototype.push);
  assert.equal(Object.hasOwn(namespace, 'releaseId'), false);
  dom.window.close();
});

test('generated fallback initializes fields through a non-configurable namespace root', () => {
  const dom = new JSDOM('<!doctype html>', { runScripts: 'outside-only' });
  dom.window.TextEncoder = TextEncoder;
  dom.window.TextDecoder = TextDecoder;
  const namespace = { que: [], boot: {} };
  Object.defineProperty(dom.window, 'tsjs', {
    configurable: false,
    enumerable: true,
    value: namespace,
    writable: false,
  });

  dom.window.eval(source);

  assert.equal(dom.window.tsjs, namespace);
  assert.equal(dom.window.tsjs.releaseId, manifest.releaseId);
  assert.equal(dom.window.tsjs._internal.reason, 'bundle_partial');
  assert.equal(Object.isFrozen(dom.window.tsjs.que), true);
  dom.window.close();
});
