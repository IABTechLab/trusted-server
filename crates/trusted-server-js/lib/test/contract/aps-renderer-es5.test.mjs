import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';

const corpus = JSON.parse(
  await readFile(new URL('../fixtures/aps-renderer-v1-corpus.json', import.meta.url), 'utf8')
);
const goldenEnvelope = JSON.parse(
  await readFile(new URL('../fixtures/aps-renderer-v1.json', import.meta.url), 'utf8')
);
const validatorUrl = new URL(
  '../../../../trusted-server-core/src/integrations/generated/aps_renderer_validator_v1.js',
  import.meta.url
);
const validatorSource = await readFile(validatorUrl, 'utf8');
const bootstrapDocument = await readFile(
  new URL(
    '../../../../trusted-server-core/src/integrations/generated/aps_renderer_bootstrap_v1.html',
    import.meta.url
  ),
  'utf8'
);

function setPath(root, path, value) {
  let parent = root;
  for (const segment of path.slice(0, -1)) parent = parent[segment];
  parent[path.at(-1)] = value;
}

function deletePath(root, path) {
  let parent = root;
  for (const segment of path.slice(0, -1)) parent = parent[segment];
  delete parent[path.at(-1)];
}

function encodeBytes(value) {
  return Buffer.from(value).toString('base64');
}

function materialize(vector) {
  const descriptor = structuredClone(corpus.baseDescriptor);
  const envelope = structuredClone(goldenEnvelope);
  const operation = vector.operation;
  let encodedEnvelope;

  switch (operation.kind) {
    case 'none':
      break;
    case 'descriptor-delete':
      delete descriptor[operation.field];
      break;
    case 'descriptor-set':
      descriptor[operation.field] = operation.value;
      break;
    case 'descriptor-repeat':
      descriptor[operation.field] =
        operation.unit.repeat(operation.count) + (operation.suffix ?? '');
      break;
    case 'bid-id-repeat': {
      const value = operation.unit.repeat(operation.count) + (operation.suffix ?? '');
      descriptor.bidId = value;
      setPath(envelope, ['seatbid', 0, 'bid', 0, 'id'], value);
      break;
    }
    case 'dimension': {
      descriptor[operation.field] = operation.value;
      setPath(
        envelope,
        ['seatbid', 0, 'bid', 0, operation.field === 'width' ? 'w' : 'h'],
        operation.value
      );
      break;
    }
    case 'dimensions':
      descriptor.width = operation.width;
      descriptor.height = operation.height;
      setPath(envelope, ['seatbid', 0, 'bid', 0, 'w'], operation.width);
      setPath(envelope, ['seatbid', 0, 'bid', 0, 'h'], operation.height);
      break;
    case 'creative-url':
      descriptor.creativeUrl = operation.value;
      setPath(envelope, ['seatbid', 0, 'bid', 0, 'ext', 'creativeurl'], operation.value);
      break;
    case 'creative-url-bytes': {
      const prefix = 'https://creative.example/';
      const value = prefix + 'a'.repeat(operation.bytes - prefix.length);
      descriptor.creativeUrl = value;
      setPath(envelope, ['seatbid', 0, 'bid', 0, 'ext', 'creativeurl'], value);
      break;
    }
    case 'aax-literal':
      encodedEnvelope = operation.value;
      break;
    case 'aax-bytes':
      encodedEnvelope = encodeBytes(Uint8Array.from(operation.values));
      break;
    case 'aax-raw-json':
      encodedEnvelope = encodeBytes(operation.value);
      break;
    case 'aax-decoded-bytes': {
      const serialized = JSON.stringify(envelope);
      assert.ok(serialized.length <= operation.bytes, vector.id);
      encodedEnvelope = encodeBytes(serialized + ' '.repeat(operation.bytes - serialized.length));
      break;
    }
    case 'aax-raw-price': {
      const serialized = JSON.stringify(envelope);
      const raw = serialized.replace('"price":1.23', `"price":${operation.value}`);
      assert.notEqual(raw, serialized, vector.id);
      encodedEnvelope = encodeBytes(raw);
      break;
    }
    case 'envelope-set':
      setPath(envelope, operation.path, operation.value);
      break;
    case 'envelope-delete':
      deletePath(envelope, operation.path);
      break;
    case 'duplicate-seat':
      envelope.seatbid.push(structuredClone(envelope.seatbid[0]));
      break;
    case 'duplicate-bid':
      envelope.seatbid[0].bid.push(structuredClone(envelope.seatbid[0].bid[0]));
      break;
    default:
      throw new Error(`unknown APS renderer corpus operation: ${operation.kind}`);
  }

  descriptor.aaxResponse = encodedEnvelope ?? encodeBytes(JSON.stringify(envelope));
  return descriptor;
}

test('the exact generated ES5 validator matches every shared corpus vector', () => {
  const context = vm.createContext({
    URL,
    TextEncoder,
    TextDecoder,
    atob,
    btoa,
    inputJson: '',
    publisherOrigin: corpus.publisherOrigin,
  });
  vm.runInContext(validatorSource, context, {
    filename: 'aps_renderer_validator_v1.js',
  });

  for (const vector of corpus.vectors) {
    context.inputJson = JSON.stringify(materialize(vector));
    const actual = vm.runInContext(
      'classifyApsRendererV1(JSON.parse(inputJson), publisherOrigin)',
      context
    );
    assert.equal(actual, vector.expected, vector.id);
  }
});

test('the embedded validator remains ES5 syntax', () => {
  assert.doesNotMatch(validatorSource, /=>|\b(?:const|let|class)\b|\?\.|\?\?/);
});

function bootstrapScript() {
  const match =
    /^<!doctype html>\n<meta charset="utf-8">\n<script>\n([\s\S]+)\n<\/script>\n$/u.exec(
      bootstrapDocument
    );
  assert.ok(match, 'bootstrap should have one exact inert document shell');
  return match[1];
}

function executeBootstrap(hash = '#b1_AAECAwQFBgcICQoLDA0ODw') {
  const parent = {
    messages: [],
    postMessage(message, origin) {
      this.messages.push({ message, origin });
    },
  };
  const replacements = [];
  let listener;
  const context = vm.createContext({
    URL,
    TextEncoder,
    document: { referrer: 'https://publisher.example/article' },
    history: { replaceState() {} },
    location: {
      hash,
      pathname: '/integrations/aps/renderer/v1',
      search: '',
      replace(value) {
        replacements.push(value);
      },
    },
    parent,
    addEventListener(type, callback) {
      assert.equal(type, 'message');
      listener = callback;
    },
    removeEventListener(type, callback) {
      assert.equal(type, 'message');
      if (listener === callback) listener = undefined;
    },
  });
  vm.runInContext(bootstrapScript(), context, {
    filename: 'aps_renderer_bootstrap_v1.html',
  });
  return {
    parent,
    replacements,
    dispatch(event) {
      listener?.(event);
    },
  };
}

test('the generated renderer response is an ES5 descriptor-free bootstrap', () => {
  const source = bootstrapScript();
  assert.doesNotMatch(source, /=>|\b(?:const|let|class)\b|\?\.|\?\?/);
  assert.doesNotMatch(
    source,
    /aaxResponse|accountId|bidId|creativeId|creativeUrl|runner|createElement|iframe|fetch|XMLHttpRequest/i
  );
  assert.match(source, /TS APS Bootstrap Ready/);
  assert.match(source, /TS APS Bootstrap Navigate/);
  assert.match(source, /data:text\/html;charset=utf-8,/);
});

test('the generated bootstrap accepts one exact parent navigation instruction', () => {
  const nonce = 'b1_AAECAwQFBgcICQoLDA0ODw';
  const harness = executeBootstrap(`#${nonce}`);
  assert.deepEqual(harness.parent.messages, [
    {
      message: JSON.stringify({
        message: 'TS APS Bootstrap Ready',
        version: 1,
        bootstrapNonce: nonce,
      }),
      origin: 'https://publisher.example',
    },
  ]);

  const containerUrl = `data:text/html;charset=utf-8,%3C!doctype%20html%3E#${nonce}`;
  const navigation = JSON.stringify({
    message: 'TS APS Bootstrap Navigate',
    version: 1,
    bootstrapNonce: nonce,
    containerUrl,
  });
  harness.dispatch({
    source: harness.parent,
    origin: 'https://publisher.example',
    data: navigation,
    ports: [],
  });
  harness.dispatch({
    source: harness.parent,
    origin: 'https://publisher.example',
    data: navigation,
    ports: [],
  });
  assert.deepEqual(harness.replacements, [containerUrl]);
});

test('the generated bootstrap accepts the exact container URL boundary', () => {
  const nonce = `b1_${'a'.repeat(22)}`;
  const prefix = 'data:text/html;charset=utf-8,';
  const suffix = `#${nonce}`;
  const containerUrl = `${prefix}${'a'.repeat(196_663 - prefix.length - suffix.length)}${suffix}`;
  const navigation = JSON.stringify({
    message: 'TS APS Bootstrap Navigate',
    version: 1,
    bootstrapNonce: nonce,
    containerUrl,
  });
  assert.equal(Buffer.byteLength(containerUrl), 196_663);
  assert.ok(Buffer.byteLength(navigation) <= 196_800);
  const harness = executeBootstrap(`#${nonce}`);

  harness.dispatch({
    source: harness.parent,
    origin: 'https://publisher.example',
    data: navigation,
    ports: [],
  });

  assert.deepEqual(harness.replacements, [containerUrl]);
});

test('the generated bootstrap rejects malformed, misbound, and oversized navigation', () => {
  const nonce = 'b1_AAECAwQFBgcICQoLDA0ODw';
  const containerUrl = `data:text/html;charset=utf-8,%3C!doctype%20html%3E#${nonce}`;
  const containerPrefix = 'data:text/html;charset=utf-8,';
  const containerSuffix = `#${nonce}`;
  const overlongContainerUrl = `${containerPrefix}${'a'.repeat(
    196_664 - containerPrefix.length - containerSuffix.length
  )}${containerSuffix}`;
  const valid = {
    message: 'TS APS Bootstrap Navigate',
    version: 1,
    bootstrapNonce: nonce,
    containerUrl,
  };
  const cases = [
    { source: {}, origin: 'https://publisher.example', data: JSON.stringify(valid), ports: [] },
    { source: null, origin: 'https://attacker.example', data: JSON.stringify(valid), ports: [] },
    {
      source: null,
      origin: 'https://publisher.example',
      data: JSON.stringify({ ...valid, bootstrapNonce: `b1_${'z'.repeat(22)}` }),
      ports: [],
    },
    {
      source: null,
      origin: 'https://publisher.example',
      data: JSON.stringify({ ...valid, extra: true }),
      ports: [],
    },
    {
      source: null,
      origin: 'https://publisher.example',
      data: JSON.stringify(valid).replace('"version":1', '"version":1,"version":1'),
      ports: [],
    },
    {
      source: null,
      origin: 'https://publisher.example',
      data: JSON.stringify(valid),
      ports: [{}],
    },
    {
      source: null,
      origin: 'https://publisher.example',
      data: JSON.stringify({
        ...valid,
        containerUrl: overlongContainerUrl,
      }),
      ports: [],
    },
    {
      source: null,
      origin: 'https://publisher.example',
      data: 'x'.repeat(196_801),
      ports: [],
    },
  ];

  for (const candidate of cases) {
    const harness = executeBootstrap(`#${nonce}`);
    harness.dispatch({
      ...candidate,
      source: candidate.source === null ? harness.parent : candidate.source,
    });
    assert.deepEqual(harness.replacements, []);
  }
  assert.deepEqual(executeBootstrap('#n1_AAECAwQFBgcICQoLDA0ODw').parent.messages, []);
});
