import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { TextDecoder, TextEncoder } from 'node:util';

import { JSDOM } from 'jsdom';

const dist = path.resolve(import.meta.dirname, '../../../dist');
const manifest = JSON.parse(readFileSync(path.join(dist, 'tsjs-release-v1.json'), 'utf8'));
const source = readFileSync(path.join(dist, 'gpt-bootstrap-fallback.js'), 'utf8');
const trustedCriticalSrc = `/static/tsjs=tsjs-unified.min.js?v=${'d'.repeat(64)}`;

function documentWithCriticalArtifact() {
  return new JSDOM(
    `<!doctype html><script id="trustedserver-js" src="${trustedCriticalSrc}"></script>`,
    { runScripts: 'outside-only', url: 'https://publisher.example/page' }
  );
}

const untrustedArtifactDocuments = [
  {
    state: 'no critical tag',
    create: () =>
      new JSDOM('<!doctype html>', {
        runScripts: 'outside-only',
        url: 'https://publisher.example/page',
      }),
  },
  {
    state: 'malformed critical tag',
    create: () =>
      new JSDOM(
        '<!doctype html><script id="trustedserver-js" src="/static/tsjs=tsjs-unified.min.js?v=malformed"></script>',
        { runScripts: 'outside-only', url: 'https://publisher.example/page' }
      ),
  },
];

const namespaceStates = [
  {
    state: 'absent namespace',
    install: () => undefined,
  },
  {
    state: 'primitive namespace',
    install: (browser) => {
      Object.defineProperty(browser, 'tsjs', {
        configurable: true,
        enumerable: false,
        value: 17,
        writable: false,
      });
    },
  },
  {
    state: 'accessor namespace',
    install: (browser) => {
      const get = () => 'publisher';
      const set = () => undefined;
      Object.defineProperty(browser, 'tsjs', {
        configurable: true,
        enumerable: false,
        get,
        set,
      });
    },
  },
  {
    state: 'object namespace descriptor',
    install: (browser) => {
      const namespace = {};
      Object.defineProperty(namespace, 'publisher', {
        configurable: false,
        enumerable: false,
        value: 'retained',
        writable: false,
      });
      Object.defineProperty(browser, 'tsjs', {
        configurable: true,
        enumerable: false,
        value: namespace,
        writable: false,
      });
    },
  },
];

for (const artifact of untrustedArtifactDocuments) {
  for (const namespace of namespaceStates) {
    test(`generated fallback preserves the exact ${namespace.state} with ${artifact.state}`, () => {
      const dom = artifact.create();
      dom.window.TextEncoder = TextEncoder;
      dom.window.TextDecoder = TextDecoder;
      namespace.install(dom.window);
      const before = Object.getOwnPropertyDescriptor(dom.window, 'tsjs');
      const beforeNamespaceDescriptors =
        before && 'value' in before && typeof before.value === 'object' && before.value !== null
          ? Object.getOwnPropertyDescriptors(before.value)
          : undefined;

      dom.window.eval(source);

      assert.deepEqual(Object.getOwnPropertyDescriptor(dom.window, 'tsjs'), before);
      if (beforeNamespaceDescriptors) {
        assert.deepEqual(
          Object.getOwnPropertyDescriptors(before.value),
          beforeNamespaceDescriptors
        );
      }
      dom.window.close();
    });
  }
}

test('generated fallback uses the independently captured critical source when manifest source is missing', () => {
  const dom = documentWithCriticalArtifact();
  dom.window.TextEncoder = TextEncoder;
  dom.window.TextDecoder = TextDecoder;
  dom.window.tsjs = {
    que: [],
    boot: {
      manifest: {
        version: 1,
        releaseId: 'b'.repeat(64),
        integrations: [],
      },
    },
  };

  dom.window.eval(source);

  assert.deepEqual(JSON.parse(JSON.stringify(dom.window.tsjs.boot.manifest)), {
    version: 1,
    releaseId: manifest.releaseId,
    criticalSrc: trustedCriticalSrc,
    integrations: [],
  });
  dom.window.close();
});

test('generated fallback uses the independently captured critical source when manifest source is malformed', () => {
  const dom = documentWithCriticalArtifact();
  dom.window.TextEncoder = TextEncoder;
  dom.window.TextDecoder = TextDecoder;
  dom.window.tsjs = {
    que: [],
    boot: {
      manifest: {
        version: 1,
        releaseId: 'b'.repeat(64),
        criticalSrc: `${trustedCriticalSrc}&publisher=1`,
        integrations: [],
      },
    },
  };

  dom.window.eval(source);

  assert.deepEqual(JSON.parse(JSON.stringify(dom.window.tsjs.boot.manifest)), {
    version: 1,
    releaseId: manifest.releaseId,
    criticalSrc: trustedCriticalSrc,
    integrations: [],
  });
  dom.window.close();
});

test('generated fallback leaves the namespace unclaimed without a trusted critical source', () => {
  const dom = new JSDOM('<!doctype html>', {
    runScripts: 'outside-only',
    url: 'https://publisher.example/page',
  });
  dom.window.TextEncoder = TextEncoder;
  dom.window.TextDecoder = TextDecoder;
  const boot = {
    manifest: {
      version: 1,
      releaseId: 'b'.repeat(64),
      integrations: [],
    },
  };
  const namespace = { que: [], boot };
  dom.window.tsjs = namespace;

  dom.window.eval(source);

  assert.equal(dom.window.tsjs, namespace);
  assert.equal(dom.window.tsjs.boot, boot);
  assert.equal(Object.hasOwn(namespace, 'releaseId'), false);
  assert.equal(Object.hasOwn(namespace, '_internal'), false);
  dom.window.close();
});

test('generated fallback bytes are stamped, executable, and add no callable global', async () => {
  assert.equal(source.includes('__TSJS_RELEASE_ID_SENTINEL_V1__'), false);
  assert.equal(source.split(manifest.releaseId).length - 1, 1);
  const dom = documentWithCriticalArtifact();
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
  const dom = documentWithCriticalArtifact();
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
  const dom = documentWithCriticalArtifact();
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
