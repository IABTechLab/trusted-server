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
const apsSource = await readFile(
  new URL('../../../../trusted-server-core/src/integrations/aps.rs', import.meta.url),
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
      encodedEnvelope = encodeBytes(
        serialized + ' '.repeat(operation.bytes - serialized.length)
      );
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

test('the exact embedded ES5 validator matches every shared corpus vector', () => {
  assert.match(
    apsSource,
    /include_str!\("generated\/aps_renderer_validator_v1\.js"\)/,
    'Rust should embed the generated validator file directly'
  );

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
