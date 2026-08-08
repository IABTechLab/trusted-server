import { describe, expect, it, vi } from 'vitest';

import corpusFixture from '../../fixtures/aps-renderer-v1-corpus.json';
import envelope from '../../fixtures/aps-renderer-v1.json';
import type { ApsRendererV1 } from '../../../src/core/types';
import { classifyApsRendererV1 } from '../../../src/core/contracts/generated/renderer_validator_v1';
import {
  parseApsRendererDescriptor,
  prepareApsRenderSource,
  validateApsRenderer,
} from '../../../src/integrations/aps/render';

function nativeRunnerState(frame: HTMLIFrameElement): {
  runner: HTMLScriptElement;
  event: CustomEvent<{ aaxResponse: string; seatBidId: string }>;
} {
  const runner = frame.contentDocument?.querySelector<HTMLScriptElement>('script');
  const frameWindow = frame.contentWindow as unknown as {
    _aps: Map<string, { queue: Array<CustomEvent<{ aaxResponse: string; seatBidId: string }>> }>;
  };
  const account = frameWindow._aps.get('example-account-id');
  expect(runner).not.toBeNull();
  expect(account?.queue).toHaveLength(1);
  return { runner: runner!, event: account!.queue[0] };
}

function encodeBytes(bytes: Uint8Array): string {
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

function encodeEnvelope(value: unknown): string {
  return encodeBytes(new TextEncoder().encode(JSON.stringify(value)));
}

function encodeEnvelopeAtSize(size: number): string {
  const serialized = JSON.stringify(envelope);
  const padding = size - new TextEncoder().encode(serialized).length;
  if (padding < 0) throw new Error('requested envelope size is too small');
  return encodeBytes(new TextEncoder().encode(`${serialized}${' '.repeat(padding)}`));
}

function descriptor(overrides: Partial<ApsRendererV1> = {}): ApsRendererV1 {
  const bid = envelope.seatbid[0]!.bid[0]!;
  return {
    type: 'aps',
    version: 1,
    accountId: 'example-account-id',
    bidId: bid.id,
    creativeId: 'fictional-creative-id',
    tagType: bid.ext.tagtype as 'iframe',
    creativeUrl: bid.ext.creativeurl,
    aaxResponse: encodeEnvelope(envelope),
    width: bid.w,
    height: bid.h,
    ...overrides,
  };
}

type CorpusResult =
  'accepted' | 'descriptor_invalid' | 'invalid_dimensions' | 'dimensions_out_of_range';

interface CorpusVector {
  id: string;
  expected: CorpusResult;
  operation: Record<string, unknown>;
}

interface RendererCorpus {
  publisherOrigin: string;
  baseDescriptor: Record<string, unknown>;
  vectors: CorpusVector[];
}

interface MaterializedCorpusVector {
  id: string;
  expected: CorpusResult;
  publisherOrigin: string;
  descriptor: Record<string, unknown>;
}

const rendererCorpus = corpusFixture as unknown as RendererCorpus;

function mutableRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function jsonPathParent(
  root: unknown,
  path: readonly (string | number)[]
): { parent: unknown; key: string | number } {
  if (path.length === 0) throw new Error('corpus path should not be empty');
  let parent = root;
  for (const segment of path.slice(0, -1)) {
    if (typeof segment === 'number') {
      if (!Array.isArray(parent)) throw new Error('corpus numeric path should address an array');
      parent = parent[segment];
    } else {
      if (!mutableRecord(parent)) throw new Error('corpus string path should address an object');
      parent = parent[segment];
    }
  }
  const key = path[path.length - 1];
  if (key === undefined) throw new Error('corpus path should have a final key');
  return { parent, key };
}

function setJsonPath(root: unknown, path: readonly (string | number)[], value: unknown): void {
  const { parent, key } = jsonPathParent(root, path);
  if (typeof key === 'number') {
    if (!Array.isArray(parent)) throw new Error('corpus numeric key should address an array');
    parent[key] = value;
    return;
  }
  if (!mutableRecord(parent)) throw new Error('corpus string key should address an object');
  parent[key] = value;
}

function deleteJsonPath(root: unknown, path: readonly (string | number)[]): void {
  const { parent, key } = jsonPathParent(root, path);
  if (typeof key !== 'string' || !mutableRecord(parent)) {
    throw new Error('corpus delete should address an object field');
  }
  delete parent[key];
}

function operationString(operation: Record<string, unknown>, field: string): string {
  const value = operation[field];
  if (typeof value !== 'string') throw new Error(`corpus ${field} should be a string`);
  return value;
}

function operationNumber(operation: Record<string, unknown>, field: string): number {
  const value = operation[field];
  if (typeof value !== 'number') throw new Error(`corpus ${field} should be a number`);
  return value;
}

function operationPath(operation: Record<string, unknown>): Array<string | number> {
  const value = operation.path;
  if (
    !Array.isArray(value) ||
    !value.every((segment) => typeof segment === 'string' || typeof segment === 'number')
  ) {
    throw new Error('corpus path should contain only string and number segments');
  }
  return value;
}

function materializeCorpusVector(vector: CorpusVector): MaterializedCorpusVector {
  const descriptor = structuredClone(rendererCorpus.baseDescriptor);
  const decodedEnvelope = structuredClone(envelope) as unknown;
  const operation = vector.operation;
  const kind = operationString(operation, 'kind');
  let encodedEnvelope: string | undefined;

  switch (kind) {
    case 'none':
      break;
    case 'descriptor-delete':
      delete descriptor[operationString(operation, 'field')];
      break;
    case 'descriptor-set':
      descriptor[operationString(operation, 'field')] = operation.value;
      break;
    case 'descriptor-repeat': {
      const repeated =
        operationString(operation, 'unit').repeat(operationNumber(operation, 'count')) +
        (typeof operation.suffix === 'string' ? operation.suffix : '');
      descriptor[operationString(operation, 'field')] = repeated;
      break;
    }
    case 'bid-id-repeat': {
      const repeated =
        operationString(operation, 'unit').repeat(operationNumber(operation, 'count')) +
        (typeof operation.suffix === 'string' ? operation.suffix : '');
      descriptor.bidId = repeated;
      setJsonPath(decodedEnvelope, ['seatbid', 0, 'bid', 0, 'id'], repeated);
      break;
    }
    case 'dimension': {
      const field = operationString(operation, 'field');
      if (field !== 'width' && field !== 'height') {
        throw new Error('corpus dimension field should be width or height');
      }
      descriptor[field] = operation.value;
      setJsonPath(
        decodedEnvelope,
        ['seatbid', 0, 'bid', 0, field === 'width' ? 'w' : 'h'],
        operation.value
      );
      break;
    }
    case 'dimensions':
      descriptor.width = operation.width;
      descriptor.height = operation.height;
      setJsonPath(decodedEnvelope, ['seatbid', 0, 'bid', 0, 'w'], operation.width);
      setJsonPath(decodedEnvelope, ['seatbid', 0, 'bid', 0, 'h'], operation.height);
      break;
    case 'creative-url': {
      const value = operationString(operation, 'value');
      descriptor.creativeUrl = value;
      setJsonPath(decodedEnvelope, ['seatbid', 0, 'bid', 0, 'ext', 'creativeurl'], value);
      break;
    }
    case 'creative-url-bytes': {
      const prefix = 'https://creative.example/';
      const value = prefix + 'a'.repeat(operationNumber(operation, 'bytes') - prefix.length);
      descriptor.creativeUrl = value;
      setJsonPath(decodedEnvelope, ['seatbid', 0, 'bid', 0, 'ext', 'creativeurl'], value);
      break;
    }
    case 'aax-literal':
      encodedEnvelope = operationString(operation, 'value');
      break;
    case 'aax-bytes': {
      const values = operation.values;
      if (!Array.isArray(values) || !values.every((value) => Number.isInteger(value))) {
        throw new Error('corpus byte vector should contain integers');
      }
      encodedEnvelope = encodeBytes(Uint8Array.from(values as number[]));
      break;
    }
    case 'aax-raw-json':
      encodedEnvelope = encodeBytes(new TextEncoder().encode(operationString(operation, 'value')));
      break;
    case 'aax-decoded-bytes': {
      const serialized = JSON.stringify(decodedEnvelope);
      const target = operationNumber(operation, 'bytes');
      if (serialized.length > target) throw new Error('corpus decoded size is below fixture size');
      encodedEnvelope = encodeBytes(
        new TextEncoder().encode(serialized + ' '.repeat(target - serialized.length))
      );
      break;
    }
    case 'aax-raw-price': {
      const serialized = JSON.stringify(decodedEnvelope);
      const price = operationString(operation, 'value');
      const raw = serialized.replace('"price":1.23', `"price":${price}`);
      if (raw === serialized) throw new Error('corpus should replace the fixture price');
      encodedEnvelope = encodeBytes(new TextEncoder().encode(raw));
      break;
    }
    case 'envelope-set':
      setJsonPath(decodedEnvelope, operationPath(operation), operation.value);
      break;
    case 'envelope-delete':
      deleteJsonPath(decodedEnvelope, operationPath(operation));
      break;
    case 'duplicate-seat': {
      if (!mutableRecord(decodedEnvelope) || !Array.isArray(decodedEnvelope.seatbid)) {
        throw new Error('corpus fixture should contain seatbid');
      }
      decodedEnvelope.seatbid.push(structuredClone(decodedEnvelope.seatbid[0]));
      break;
    }
    case 'duplicate-bid': {
      const seatbid = mutableRecord(decodedEnvelope) ? decodedEnvelope.seatbid : undefined;
      const seat = Array.isArray(seatbid) ? seatbid[0] : undefined;
      const bids = mutableRecord(seat) ? seat.bid : undefined;
      if (!Array.isArray(bids)) throw new Error('corpus fixture should contain a bid array');
      bids.push(structuredClone(bids[0]));
      break;
    }
    default:
      throw new Error(`unknown APS renderer corpus operation: ${kind}`);
  }

  descriptor.aaxResponse = encodedEnvelope ?? encodeEnvelope(decodedEnvelope);
  return {
    id: vector.id,
    expected: vector.expected,
    publisherOrigin: rendererCorpus.publisherOrigin,
    descriptor,
  };
}

describe('APS renderer validation', () => {
  it('prepares a copied frozen tagged source without retaining projection input', () => {
    const input = descriptor();
    const prepared = prepareApsRenderSource(input);

    expect(prepared).toEqual(input);
    expect(prepared).not.toBe(input);
    expect(Object.isFrozen(prepared)).toBe(true);
    input.width = 1;
    expect(prepared?.width).toBe(300);
  });

  it('prepares a cached validated source after Object.freeze is poisoned', () => {
    const validated = validateApsRenderer(descriptor());
    if (!validated) throw new Error('Expected a validated renderer');
    const originalFreeze = Object.freeze;
    let prepared: ReturnType<typeof prepareApsRenderSource> | undefined;
    let thrown: unknown;
    Object.freeze = function poisonedFreeze() {
      throw new Error('poisoned Object.freeze');
    };
    try {
      prepared = prepareApsRenderSource(validated);
    } catch (error) {
      thrown = error;
    } finally {
      Object.freeze = originalFreeze;
    }

    expect(thrown).toBeUndefined();
    expect(prepared).toBe(validated);
    expect(Object.isFrozen(prepared)).toBe(true);
  });

  it('matches every shared cross-language contract vector', () => {
    for (const vector of rendererCorpus.vectors.map(materializeCorpusVector)) {
      const actual = classifyApsRendererV1(vector.descriptor, vector.publisherOrigin);
      expect(actual, vector.id).toBe(vector.expected);
    }
  });

  it('consumes the shared fictional golden envelope and supports an omitted creative ID', () => {
    const withCreativeId = descriptor();
    const withoutCreativeId = descriptor();
    delete withoutCreativeId.creativeId;

    expect(validateApsRenderer(withCreativeId)).toEqual(withCreativeId);
    expect(validateApsRenderer(withoutCreativeId)).toEqual(withoutCreativeId);
  });

  it('caches an immutable validated descriptor for the same publisher origin', () => {
    const input = descriptor();
    const atobSpy = vi.spyOn(window, 'atob');

    const first = validateApsRenderer(input, 'https://publisher.example');
    input.width = 728;
    const second = validateApsRenderer(first, 'https://publisher.example');

    expect(first?.width).toBe(300);
    expect(first).toBe(second);
    expect(Object.isFrozen(first)).toBe(true);
    expect(atobSpy).toHaveBeenCalledTimes(1);
    expect(() => {
      first!.width = 728;
    }).toThrow(TypeError);

    expect(validateApsRenderer(first, 'https://other-publisher.example')).toBeDefined();
    expect(atobSpy).toHaveBeenCalledTimes(2);
    atobSpy.mockRestore();
  });

  it('keeps auction parsing structural and leaves complete trust validation to render time', () => {
    const renderer = descriptor({ aaxResponse: 'not-base64' });

    expect(parseApsRendererDescriptor(renderer)).toEqual(renderer);
    expect(validateApsRenderer(renderer)).toBeUndefined();
  });

  it.each([
    ['unknown root field', { ...envelope, id: 'forbidden' }],
    ['sibling seat', { seatbid: [...envelope.seatbid, envelope.seatbid[0]] }],
    [
      'sibling bid',
      { seatbid: [{ bid: [...envelope.seatbid[0]!.bid, envelope.seatbid[0]!.bid[0]!] }] },
    ],
    [
      'markup',
      {
        seatbid: [
          { bid: [{ ...envelope.seatbid[0]!.bid[0]!, adm: '<script>forbidden()</script>' }] },
        ],
      },
    ],
    [
      'notification',
      { seatbid: [{ bid: [{ ...envelope.seatbid[0]!.bid[0]!, nurl: 'https://notify.example' }] }] },
    ],
    [
      'unknown extension',
      {
        seatbid: [
          {
            bid: [
              {
                ...envelope.seatbid[0]!.bid[0]!,
                ext: { ...envelope.seatbid[0]!.bid[0]!.ext, userSyncs: [] },
              },
            ],
          },
        ],
      },
    ],
  ])('rejects an envelope containing %s', (_name, invalidEnvelope) => {
    expect(
      validateApsRenderer(descriptor({ aaxResponse: encodeEnvelope(invalidEnvelope) }))
    ).toBeUndefined();
  });

  it.each([
    ['bid ID', { bidId: 'another-bid' }],
    ['width', { width: 728 }],
    ['height', { height: 90 }],
    ['creative URL', { creativeUrl: 'https://other.example/render' }],
    ['tag type', { tagType: 'script' as const }],
  ])('rejects a descriptor/envelope %s mismatch', (_name, override) => {
    expect(validateApsRenderer(descriptor(override))).toBeUndefined();
  });

  it.each([
    'not-base64',
    'e30',
    '====',
    'Zh==',
    btoa(String.fromCharCode(0xc3, 0x28)),
    btoa('{not json}'),
  ])('rejects invalid base64, UTF-8, JSON, or non-canonical padding', (aaxResponse) => {
    expect(validateApsRenderer(descriptor({ aaxResponse }))).toBeUndefined();
  });

  it('rejects non-canonical trailing bits that decode to the valid envelope', () => {
    const canonical = encodeBytes(new TextEncoder().encode(`${JSON.stringify(envelope)} `));
    const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
    const finalDataIndex = canonical.length - 3;
    const canonicalIndex = alphabet.indexOf(canonical[finalDataIndex]!);
    const nonCanonical = `${canonical.slice(0, finalDataIndex)}${alphabet[canonicalIndex + 1]!}==`;

    expect(atob(nonCanonical)).toBe(atob(canonical));
    expect(validateApsRenderer(descriptor({ aaxResponse: canonical }))).toBeDefined();
    expect(validateApsRenderer(descriptor({ aaxResponse: nonCanonical }))).toBeUndefined();
  });

  it.each([
    'http://creative.example/render',
    'https://user:password@creative.example/render',
    `${window.location.origin}/creative`,
  ])('rejects an unsafe creative URL', (creativeUrl) => {
    const invalidEnvelope = structuredClone(envelope);
    invalidEnvelope.seatbid[0]!.bid[0]!.ext.creativeurl = creativeUrl;
    expect(
      validateApsRenderer(descriptor({ creativeUrl, aaxResponse: encodeEnvelope(invalidEnvelope) }))
    ).toBeUndefined();
  });

  it('rejects unknown descriptor fields and version mismatches', () => {
    expect(
      parseApsRendererDescriptor({ ...descriptor(), adm: '<div>forbidden</div>' })
    ).toBeUndefined();
    expect(parseApsRendererDescriptor({ ...descriptor(), version: 2 })).toBeUndefined();
  });

  it('enforces account and creative ID UTF-8 byte limits', () => {
    expect(validateApsRenderer(descriptor({ accountId: 'é'.repeat(512) }))).toBeDefined();
    expect(validateApsRenderer(descriptor({ accountId: `${'é'.repeat(512)}x` }))).toBeUndefined();
    expect(validateApsRenderer(descriptor({ creativeId: 'é'.repeat(512) }))).toBeDefined();
    expect(validateApsRenderer(descriptor({ creativeId: `${'é'.repeat(512)}x` }))).toBeUndefined();
  });

  it('enforces the creative URL UTF-8 byte limit', () => {
    const prefix = 'https://creative.example/';
    const atLimit = `${prefix}${'a'.repeat(4096 - prefix.length)}`;
    const overLimit = `${atLimit}x`;
    const atLimitEnvelope = structuredClone(envelope);
    atLimitEnvelope.seatbid[0]!.bid[0]!.ext.creativeurl = atLimit;
    const overLimitEnvelope = structuredClone(envelope);
    overLimitEnvelope.seatbid[0]!.bid[0]!.ext.creativeurl = overLimit;

    expect(
      validateApsRenderer(
        descriptor({ creativeUrl: atLimit, aaxResponse: encodeEnvelope(atLimitEnvelope) })
      )
    ).toBeDefined();
    expect(
      validateApsRenderer(
        descriptor({ creativeUrl: overLimit, aaxResponse: encodeEnvelope(overLimitEnvelope) })
      )
    ).toBeUndefined();
  });

  it('accepts the maximum decoded envelope and rejects one byte over', () => {
    const atLimit = encodeEnvelopeAtSize(256 * 1024);
    const overLimit = encodeEnvelopeAtSize(256 * 1024 + 1);

    expect(atLimit).toHaveLength(349528);
    expect(overLimit).toHaveLength(349528);
    expect(validateApsRenderer(descriptor({ aaxResponse: atLimit }))).toBeDefined();
    expect(validateApsRenderer(descriptor({ aaxResponse: overLimit }))).toBeUndefined();
    expect(
      parseApsRendererDescriptor(descriptor({ aaxResponse: `${atLimit}AAAA` }))
    ).toBeUndefined();
  });
});
