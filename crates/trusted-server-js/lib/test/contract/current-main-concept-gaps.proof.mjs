import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { JSDOM } from 'jsdom';
import { createServer } from 'vite';

const libRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

const GLOBAL_NAMES = [
  'CustomEvent',
  'Document',
  'Element',
  'Event',
  'HTMLIFrameElement',
  'HTMLElement',
  'MessageEvent',
  'MutationObserver',
  'Node',
  'URL',
  'Window',
  'document',
  'getComputedStyle',
  'history',
  'location',
  'navigator',
  'window',
];

async function withBrowserModule(modulePath, run) {
  const dom = new JSDOM('<!doctype html><html><body></body></html>', {
    pretendToBeVisual: true,
    url: 'https://publisher.example/article',
  });
  const previous = new Map();
  for (const name of GLOBAL_NAMES) {
    previous.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
    const value =
      name === 'getComputedStyle' ? dom.window.getComputedStyle.bind(dom.window) : dom.window[name];
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
  }
  previous.set('crypto', Object.getOwnPropertyDescriptor(globalThis, 'crypto'));
  Object.defineProperty(globalThis, 'crypto', {
    configurable: true,
    value: dom.window.crypto,
  });

  const server = await createServer({
    appType: 'custom',
    configFile: false,
    logLevel: 'silent',
    root: libRoot,
    server: { middlewareMode: true },
  });
  try {
    const loaded = await server.ssrLoadModule(modulePath);
    return await run({ dom, loaded });
  } finally {
    await server.close();
    dom.window.close();
    for (const [name, descriptor] of previous) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor);
      else Reflect.deleteProperty(globalThis, name);
    }
  }
}

test('RCJ-TRACE-01 exposes the bounded public render-trace API', async () => {
  await withBrowserModule('/src/core/index.ts', ({ dom }) => {
    const renderTrace = dom.window.tsjs?.diagnostics?.renderTrace;
    assert.deepEqual(Reflect.ownKeys(renderTrace ?? {}).sort(), [
      'current',
      'history',
      'subscribe',
    ]);
    assert.equal(Object.isFrozen(renderTrace), true);
  });
});

test('RCJ-GPT-04 resizes only the authenticated collapsed shell after posting', async () => {
  await withBrowserModule('/src/integrations/gpt/index.ts', ({ dom }) => {
    const wrapper = dom.window.document.createElement('div');
    wrapper.id = 'slot-a';
    wrapper.style.width = '1px';
    wrapper.style.height = '1px';
    const frame = dom.window.document.createElement('iframe');
    frame.width = '1';
    frame.height = '1';
    frame.style.width = '1px';
    frame.style.height = '1px';
    wrapper.append(frame);
    dom.window.document.body.append(wrapper);
    dom.window.tsjs = {
      adSlots: [{ id: 'slot-a', div_id: 'slot-a', formats: [[300, 250]] }],
      bids: { 'slot-a': { adm: '<div>creative</div>', h: 250, hb_adid: 'ad-a', w: 300 } },
    };

    const responses = [];
    const event = new dom.window.MessageEvent('message', {
      data: { adId: 'ad-a', message: 'Prebid Request' },
      ports: [{ postMessage: (response) => responses.push(JSON.parse(response)) }],
      source: frame.contentWindow,
    });
    dom.window.dispatchEvent(event);

    assert.equal(responses.length, 1, 'the authenticated TS response should be posted');
    assert.equal(frame.style.width, '300px');
    assert.equal(frame.style.height, '250px');
    assert.equal(wrapper.style.width, '300px');
    assert.equal(wrapper.style.height, '250px');
  });
});

function apsDescriptor(window) {
  const bid = {
    ext: { creativeurl: 'https://creative.example/render', tagtype: 'iframe' },
    h: 250,
    id: 'bid-a',
    price: 1,
    w: 300,
  };
  return {
    aaxResponse: window.btoa(JSON.stringify({ seatbid: [{ bid: [bid] }] })),
    accountId: 'account-a',
    bidId: bid.id,
    creativeId: 'creative-a',
    creativeUrl: bid.ext.creativeurl,
    height: bid.h,
    tagType: bid.ext.tagtype,
    type: 'aps',
    version: 1,
    width: bid.w,
  };
}

test('RCJ-APS-03 waits for an APS callback instead of script load', async () => {
  const sourcePath = path.resolve(libRoot, '../../trusted-server-core/src/integrations/aps.rs');
  const rust = await readFile(sourcePath, 'utf8');
  const documentMatch = /const APS_RENDERER_DOCUMENT: &str = r#"([\s\S]*?)"#;/.exec(rust);
  assert.ok(documentMatch, 'current main should expose the renderer document under test');
  const scriptMatch = /<script>([\s\S]*?)<\/script>/.exec(documentMatch[1]);
  assert.ok(scriptMatch, 'renderer document should contain executable code');

  const nonce = 'a'.repeat(22);
  const dom = new JSDOM('<!doctype html><html><head></head><body></body></html>', {
    runScripts: 'outside-only',
    url: `https://publisher.example/integrations/aps/renderer#tsaps=${nonce}`,
  });
  const posts = [];
  dom.window.postMessage = (message) => posts.push(message);
  dom.window.eval(scriptMatch[1]);
  dom.window.dispatchEvent(
    new dom.window.MessageEvent('message', {
      data: { nonce, renderer: apsDescriptor(dom.window) },
      source: dom.window,
    })
  );
  const runner = dom.window.document.querySelector(
    'script[src="https://client.aps.amazon-adsystem.com/prebid-creative.js"]'
  );
  assert.ok(runner, 'valid input should reach APS runner insertion');
  runner.onload();

  assert.equal(
    posts.some((message) => message?.message === 'trusted-server/aps/renderer-ready'),
    false,
    'runner load is progress; only the APS success callback may complete rendering'
  );
  dom.window.close();
});

test('RCJ-APS-04 applies winning dimensions without document clipping', async () => {
  await withBrowserModule('/src/integrations/aps/render.ts', async ({ loaded }) => {
    const owner = new JSDOM('<!doctype html><html><body></body></html>', {
      runScripts: 'outside-only',
      url: 'https://publisher.example/article',
    });
    owner.window.eval(loaded.APS_UNIVERSAL_CREATIVE_RENDERER);
    owner.window.setTimeout = () => 1;
    owner.window.clearTimeout = () => undefined;
    const pending = owner.window.render(
      {
        apsRenderer: { height: 250, width: 300 },
        rendererUrl: 'https://publisher.example/integrations/aps/renderer',
      },
      undefined,
      owner.window
    );
    const frame = owner.window.document.querySelector('iframe');
    assert.ok(frame, 'the dynamic owner should insert the renderer frame');
    assert.equal(owner.window.document.documentElement.style.margin, '0px');
    assert.equal(owner.window.document.documentElement.style.padding, '0px');
    assert.equal(owner.window.document.documentElement.style.overflow, 'hidden');
    assert.equal(owner.window.document.body.style.margin, '0px');
    assert.equal(owner.window.document.body.style.padding, '0px');
    assert.equal(owner.window.document.body.style.overflow, 'hidden');
    assert.equal(frame.style.width, '300px');
    assert.equal(frame.style.height, '250px');
    assert.equal(frame.style.display, 'block');
    assert.equal(frame.scrolling, 'no');
    frame.onerror();
    await assert.rejects(pending);
    owner.window.close();
  });
});

test('RCJ-QUAL-01 exposes full-package typecheck and lint gates', async () => {
  const packageJson = JSON.parse(await readFile(path.join(libRoot, 'package.json'), 'utf8'));
  assert.equal(typeof packageJson.scripts?.typecheck, 'string');
  assert.match(packageJson.scripts.typecheck, /tsc/);
  assert.match(packageJson.scripts.lint, /eslint\s+\./);
});
