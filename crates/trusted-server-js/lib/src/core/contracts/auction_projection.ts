import { parseCacheFetchPolicyV1 } from '../config';
import type {
  AdmRenderSourceV1,
  AuctionDecisionSetV1,
  AuctionSlotFailureReason,
  BidRenderSourceV1,
  BrowserAuctionBidV1,
  BrowserAuctionProjectionV1,
  CacheFetchPolicyV1,
  CacheRenderSourceV1,
  SlotAuctionDecisionV1,
} from '../types';

import { validateApsRenderer } from './aps_renderer';

export const MAX_BROWSER_AUCTION_PROJECTION_BYTES = 8 * 1024 * 1024;

export const MAX_AUCTION_RESULTS = 256;
const MAX_TARGETING_ENTRIES = 32;
const MAX_ADM_BYTES = 512 * 1024;
const MAX_URL_BYTES = 4096;
const reflectApplyIntrinsic = Reflect.apply;
const objectGetOwnPropertyNamesIntrinsic = Object.getOwnPropertyNames;
const objectKeysIntrinsic = Object.keys;
const textEncoder = new TextEncoder();
const textEncoderEncodeIntrinsic = TextEncoder.prototype.encode;
const regExpTestIntrinsic = RegExp.prototype.test;
const candidateIdPattern = /^[A-Za-z0-9_-]{12}$/;
const reservationIdPattern = /^r1_[A-Za-z0-9_-]{22}$/;
const auctionIdPattern = /^[A-Za-z0-9._:-]{1,128}$/;
const providerPattern = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const targetingKeyPattern = /^[A-Za-z0-9_]{1,20}$/;
const cacheIdPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const auctionFailureReasons = new Set<AuctionSlotFailureReason>([
  'auction_disabled',
  'consent_denied',
  'slot_not_eligible',
  'provider_timeout',
  'provider_error',
  'invalid_provider_response',
  'mediation_failed',
  'winner_not_renderable',
  'identity_generation_failed',
  'internal_error',
]);

function hasString(values: readonly string[], expected: string): boolean {
  for (let index = 0; index < values.length; index += 1) {
    if (values[index] === expected) return true;
  }
  return false;
}

export function ownDataObject(
  value: unknown,
  expectedKeys?: readonly string[]
): Record<string, unknown> | undefined {
  try {
    if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
    if (Object.getPrototypeOf(value) !== Object.prototype) return undefined;
    if (Object.getOwnPropertySymbols(value).length !== 0) return undefined;
    const names = reflectApplyIntrinsic(objectGetOwnPropertyNamesIntrinsic, Object, [
      value,
    ]) as string[];
    if (expectedKeys) {
      if (names.length !== expectedKeys.length) return undefined;
      for (let index = 0; index < expectedKeys.length; index += 1) {
        const expected = expectedKeys[index];
        if (expected === undefined || !hasString(names, expected)) return undefined;
      }
    }
    const snapshot: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    for (let index = 0; index < names.length; index += 1) {
      const name = names[index];
      if (name === undefined) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(value, name);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      snapshot[name] = descriptor.value;
    }
    return snapshot;
  } catch {
    return undefined;
  }
}

export function ownDataArray(value: unknown, maximum: number): unknown[] | undefined {
  try {
    if (!Array.isArray(value) || Object.getPrototypeOf(value) !== Array.prototype) return undefined;
    if (value.length > maximum || Object.getOwnPropertySymbols(value).length !== 0)
      return undefined;
    const names = reflectApplyIntrinsic(objectGetOwnPropertyNamesIntrinsic, Object, [
      value,
    ]) as string[];
    if (names.length !== value.length + 1 || !hasString(names, 'length')) return undefined;
    const snapshot: unknown[] = [];
    for (let index = 0; index < value.length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      snapshot.push(descriptor.value);
    }
    return snapshot;
  } catch {
    return undefined;
  }
}

function unicodeScalarCount(value: string): number | undefined {
  let scalars = 0;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return undefined;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return undefined;
    }
    scalars += 1;
  }
  return scalars;
}

function hasAsciiControl(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code <= 0x1f || code === 0x7f) return true;
  }
  return false;
}

export function validBoundedString(
  value: unknown,
  maximumBytes: number,
  options: { allowControls?: boolean; maximumScalars?: number } = {}
): value is string {
  if (typeof value !== 'string' || value.length === 0) return false;
  const scalarCount = unicodeScalarCount(value);
  return (
    scalarCount !== undefined &&
    (options.allowControls === true || !hasAsciiControl(value)) &&
    (reflectApplyIntrinsic(textEncoderEncodeIntrinsic, textEncoder, [value]) as Uint8Array)
      .length <= maximumBytes &&
    (options.maximumScalars === undefined || scalarCount <= options.maximumScalars)
  );
}

function matches(pattern: RegExp, value: string): boolean {
  return reflectApplyIntrinsic(regExpTestIntrinsic, pattern, [value]) as boolean;
}

export function validDimension(value: unknown): value is number {
  return (
    typeof value === 'number' &&
    Number.isFinite(value) &&
    Number.isInteger(value) &&
    value >= 1 &&
    value <= 4096
  );
}

export function isAuctionCandidateIdV1(value: unknown): value is string {
  return typeof value === 'string' && matches(candidateIdPattern, value);
}

export function isAuctionProviderIdV1(value: unknown): value is string {
  return typeof value === 'string' && matches(providerPattern, value);
}

function boundedJsonBytes(left: number, right: number, maximum: number): number {
  return left > maximum - right ? maximum + 1 : left + right;
}

function encodedJsonStringBytes(value: string, maximum: number): number {
  let bytes = 2;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    let encodedBytes: number;
    if (code === 0x22 || code === 0x5c) encodedBytes = 2;
    else if (code <= 0x1f) {
      encodedBytes =
        code === 0x08 || code === 0x09 || code === 0x0a || code === 0x0c || code === 0x0d ? 2 : 6;
    } else if (code <= 0x7f) encodedBytes = 1;
    else if (code <= 0x7ff) encodedBytes = 2;
    else if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (next >= 0xdc00 && next <= 0xdfff) {
        encodedBytes = 4;
        index += 1;
      } else encodedBytes = 6;
    } else if (code >= 0xdc00 && code <= 0xdfff) encodedBytes = 6;
    else encodedBytes = 3;
    bytes = boundedJsonBytes(bytes, encodedBytes, maximum);
    if (bytes > maximum) return bytes;
  }
  return bytes;
}

interface JsonMeasureSnapshot {
  readonly array: boolean;
  readonly entries: readonly Readonly<{ key: string; value: unknown }>[];
}

interface JsonMeasureFrame extends JsonMeasureSnapshot {
  readonly source: object;
  bytes: number;
  index: number;
}

function jsonPrimitiveBytes(value: unknown): number | undefined {
  if (value === null) return 4;
  if (typeof value === 'boolean') return value ? 4 : 5;
  if (typeof value === 'number' && Number.isFinite(value)) return `${value}`.length;
  return typeof value === 'string'
    ? encodedJsonStringBytes(value, MAX_BROWSER_AUCTION_PROJECTION_BYTES)
    : undefined;
}

function snapshotJsonForMeasurement(value: object): JsonMeasureSnapshot | undefined {
  const array = Array.isArray(value);
  const values = array ? ownDataArray(value, MAX_AUCTION_RESULTS) : undefined;
  if (array && !values) return undefined;
  const record = array ? undefined : ownDataObject(value);
  if (!array && !record) return undefined;
  return {
    array,
    entries: array
      ? values!.map((entry, index) => ({ key: String(index), value: entry }))
      : (reflectApplyIntrinsic(objectKeysIntrinsic, Object, [record]) as string[]).map((key) => ({
          key,
          value: record![key],
        })),
  };
}

/** Measure exact own JSON data without consulting accessors or inherited `toJSON` hooks. */
export function jsonUtf8ByteLength(value: unknown): number {
  const primitive = jsonPrimitiveBytes(value);
  if (primitive !== undefined) return primitive;
  if (typeof value !== 'object' || value === null) return Number.POSITIVE_INFINITY;
  const root = snapshotJsonForMeasurement(value);
  if (!root) return Number.POSITIVE_INFINITY;
  const memo = new WeakMap<object, number>();
  const active = new Set<object>();
  active.add(value);
  const stack: JsonMeasureFrame[] = [{ ...root, bytes: 2, index: 0, source: value }];
  while (stack.length > 0) {
    const frame = stack[stack.length - 1];
    if (!frame) return Number.POSITIVE_INFINITY;
    if (frame.index >= frame.entries.length) {
      memo.set(frame.source, frame.bytes);
      active.delete(frame.source);
      stack.pop();
      const parent = stack[stack.length - 1];
      if (!parent) return frame.bytes;
      parent.bytes = boundedJsonBytes(
        parent.bytes,
        frame.bytes,
        MAX_BROWSER_AUCTION_PROJECTION_BYTES
      );
      if (parent.bytes > MAX_BROWSER_AUCTION_PROJECTION_BYTES) return parent.bytes;
      continue;
    }
    const entry = frame.entries[frame.index];
    const entryIndex = frame.index;
    frame.index += 1;
    if (!entry) return Number.POSITIVE_INFINITY;
    const keyBytes = frame.array ? 0 : jsonPrimitiveBytes(entry.key);
    if (keyBytes === undefined) return Number.POSITIVE_INFINITY;
    frame.bytes = boundedJsonBytes(
      frame.bytes,
      (entryIndex === 0 ? 0 : 1) + (frame.array ? 0 : keyBytes + 1),
      MAX_BROWSER_AUCTION_PROJECTION_BYTES
    );
    if (frame.bytes > MAX_BROWSER_AUCTION_PROJECTION_BYTES) return frame.bytes;
    const childPrimitive = jsonPrimitiveBytes(entry.value);
    if (childPrimitive !== undefined) {
      frame.bytes = boundedJsonBytes(
        frame.bytes,
        childPrimitive,
        MAX_BROWSER_AUCTION_PROJECTION_BYTES
      );
      if (frame.bytes > MAX_BROWSER_AUCTION_PROJECTION_BYTES) return frame.bytes;
      continue;
    }
    if (typeof entry.value !== 'object' || entry.value === null || active.has(entry.value)) {
      return Number.POSITIVE_INFINITY;
    }
    const completed = memo.get(entry.value);
    if (completed !== undefined) {
      frame.bytes = boundedJsonBytes(frame.bytes, completed, MAX_BROWSER_AUCTION_PROJECTION_BYTES);
      if (frame.bytes > MAX_BROWSER_AUCTION_PROJECTION_BYTES) return frame.bytes;
      continue;
    }
    const child = snapshotJsonForMeasurement(entry.value);
    if (!child) return Number.POSITIVE_INFINITY;
    active.add(entry.value);
    stack.push({ ...child, bytes: 2, index: 0, source: entry.value });
  }
  return Number.POSITIVE_INFINITY;
}

/** Whether a value is one exact server-minted renderer reservation identity. */
export function isRendererReservationIdV1(value: unknown): value is string {
  return typeof value === 'string' && matches(reservationIdPattern, value);
}

/** Validate and copy one exact browser render-source contract. */
export function parseBidRenderSourceV1(
  value: unknown,
  cachePolicy?: Readonly<CacheFetchPolicyV1>
): BidRenderSourceV1 | undefined {
  const record = ownDataObject(value);
  if (!record || typeof record.type !== 'string') return undefined;

  if (record.type === 'aps') {
    const keys = Object.prototype.hasOwnProperty.call(record, 'creativeId')
      ? [
          'type',
          'version',
          'accountId',
          'bidId',
          'creativeId',
          'tagType',
          'creativeUrl',
          'aaxResponse',
          'width',
          'height',
        ]
      : [
          'type',
          'version',
          'accountId',
          'bidId',
          'tagType',
          'creativeUrl',
          'aaxResponse',
          'width',
          'height',
        ];
    if (!ownDataObject(value, keys)) return undefined;
    const renderer = validateApsRenderer(record);
    if (!renderer) return undefined;
    return {
      type: 'aps',
      version: 1,
      accountId: renderer.accountId,
      bidId: renderer.bidId,
      ...(renderer.creativeId === undefined ? {} : { creativeId: renderer.creativeId }),
      tagType: renderer.tagType,
      creativeUrl: renderer.creativeUrl,
      aaxResponse: renderer.aaxResponse,
      width: renderer.width,
      height: renderer.height,
    };
  }

  if (record.type === 'adm') {
    const source = ownDataObject(value, ['type', 'version', 'adm', 'width', 'height']);
    if (
      !source ||
      source.version !== 1 ||
      !validBoundedString(source.adm, MAX_ADM_BYTES, { allowControls: true }) ||
      !validDimension(source.width) ||
      !validDimension(source.height)
    ) {
      return undefined;
    }
    return {
      type: 'adm',
      version: 1,
      adm: source.adm,
      width: source.width,
      height: source.height,
    } satisfies AdmRenderSourceV1;
  }

  if (record.type === 'cache') {
    const source = ownDataObject(value, [
      'type',
      'version',
      'cacheId',
      'fetchUrl',
      'width',
      'height',
    ]);
    if (
      !source ||
      source.version !== 1 ||
      typeof source.cacheId !== 'string' ||
      !matches(cacheIdPattern, source.cacheId) ||
      !validBoundedString(source.fetchUrl, MAX_URL_BYTES) ||
      !validDimension(source.width) ||
      !validDimension(source.height) ||
      !cachePolicy
    ) {
      return undefined;
    }
    let fetchUrl: URL;
    try {
      fetchUrl = new URL(source.fetchUrl);
    } catch {
      return undefined;
    }
    if (
      fetchUrl.protocol !== 'https:' ||
      fetchUrl.username !== '' ||
      fetchUrl.password !== '' ||
      fetchUrl.hash !== '' ||
      [...fetchUrl.searchParams.keys()].length !== 1 ||
      fetchUrl.searchParams.get('uuid') !== source.cacheId ||
      fetchUrl.search !== `?uuid=${encodeURIComponent(source.cacheId)}`
    ) {
      return undefined;
    }
    let policyBase: URL;
    try {
      policyBase = new URL(cachePolicy.baseUrl);
    } catch {
      return undefined;
    }
    const expected = new URL(policyBase.href);
    expected.search = `?uuid=${encodeURIComponent(source.cacheId)}`;
    if (
      fetchUrl.origin !== policyBase.origin ||
      fetchUrl.port !== policyBase.port ||
      fetchUrl.pathname !== policyBase.pathname ||
      fetchUrl.href !== expected.href
    ) {
      return undefined;
    }
    return {
      type: 'cache',
      version: 1,
      cacheId: source.cacheId,
      fetchUrl: fetchUrl.href,
      width: source.width,
      height: source.height,
    } satisfies CacheRenderSourceV1;
  }

  return undefined;
}

/** Validate and copy one exact auction decision-set contract. */
export function parseAuctionDecisionSetV1(value: unknown): AuctionDecisionSetV1 | undefined {
  const record = ownDataObject(value, ['version', 'auctionId', 'results']);
  if (!record || record.version !== 1 || typeof record.auctionId !== 'string') return undefined;
  if (!matches(auctionIdPattern, record.auctionId)) return undefined;
  const results = ownDataArray(record.results, MAX_AUCTION_RESULTS);
  if (!results) return undefined;

  const parsed: SlotAuctionDecisionV1[] = [];
  const slots = new Set<string>();
  const candidates = new Set<string>();
  for (let index = 0; index < results.length; index += 1) {
    const raw = results[index];
    const base = ownDataObject(raw);
    if (!base || !validBoundedString(base.slot, 256) || slots.has(base.slot)) return undefined;
    slots.add(base.slot);
    if (base.outcome === 'winner') {
      const winner = ownDataObject(raw, ['slot', 'outcome', 'candidateId']);
      if (
        !winner ||
        !isAuctionCandidateIdV1(winner.candidateId) ||
        candidates.has(winner.candidateId)
      ) {
        return undefined;
      }
      candidates.add(winner.candidateId);
      parsed.push({ slot: base.slot, outcome: 'winner', candidateId: winner.candidateId });
    } else if (base.outcome === 'no_bid') {
      if (!ownDataObject(raw, ['slot', 'outcome'])) return undefined;
      parsed.push({ slot: base.slot, outcome: 'no_bid' });
    } else if (base.outcome === 'failed') {
      const failed = ownDataObject(raw, ['slot', 'outcome', 'reason']);
      if (
        !failed ||
        typeof failed.reason !== 'string' ||
        !auctionFailureReasons.has(failed.reason as AuctionSlotFailureReason)
      ) {
        return undefined;
      }
      parsed.push({
        slot: base.slot,
        outcome: 'failed',
        reason: failed.reason as AuctionSlotFailureReason,
      });
    } else {
      return undefined;
    }
  }

  return { version: 1, auctionId: record.auctionId, results: parsed };
}

function parseTargeting(value: unknown): Record<string, string> | undefined {
  const record = ownDataObject(value);
  if (!record) return undefined;
  const entries = Object.entries(record);
  if (entries.length > MAX_TARGETING_ENTRIES) return undefined;
  const targeting: Record<string, string> = {};
  entries.sort((leftEntry, rightEntry) => {
    const left = leftEntry[0];
    const right = rightEntry[0];
    return left < right ? -1 : left > right ? 1 : 0;
  });
  for (let index = 0; index < entries.length; index += 1) {
    const pair = entries[index];
    if (!pair) return undefined;
    const key = pair[0];
    const entry = pair[1];
    if (
      key === 'hb_adid' ||
      !matches(targetingKeyPattern, key) ||
      !validBoundedString(entry, 160, { maximumScalars: 40 })
    ) {
      return undefined;
    }
    Object.defineProperty(targeting, key, {
      value: entry,
      enumerable: true,
      writable: true,
      configurable: true,
    });
  }
  return targeting;
}

function parseBrowserBid(
  value: unknown,
  cachePolicy?: Readonly<CacheFetchPolicyV1>
): BrowserAuctionBidV1 | undefined {
  const bid = ownDataObject(value, [
    'candidateId',
    'slot',
    'provider',
    'upstreamBidId',
    'cpm',
    'currency',
    'targeting',
    'rendererReservationId',
    'renderSource',
  ]);
  if (
    !bid ||
    !isAuctionCandidateIdV1(bid.candidateId) ||
    !validBoundedString(bid.slot, 256) ||
    !isAuctionProviderIdV1(bid.provider) ||
    !validBoundedString(bid.upstreamBidId, 64) ||
    typeof bid.cpm !== 'number' ||
    !Number.isFinite(bid.cpm) ||
    bid.cpm < 0 ||
    bid.currency !== 'USD' ||
    !isRendererReservationIdV1(bid.rendererReservationId)
  ) {
    return undefined;
  }
  const targeting = parseTargeting(bid.targeting);
  const renderSource = parseBidRenderSourceV1(bid.renderSource, cachePolicy);
  if (!targeting || !renderSource) return undefined;
  return {
    candidateId: bid.candidateId,
    slot: bid.slot,
    provider: bid.provider,
    upstreamBidId: bid.upstreamBidId,
    cpm: bid.cpm,
    currency: 'USD',
    targeting,
    rendererReservationId: bid.rendererReservationId,
    renderSource,
  };
}

/** Validate, canonicalize, and deep-copy a complete browser auction projection. */
export function parseBrowserAuctionProjectionV1(
  value: unknown,
  cachePolicyValue?: unknown
): BrowserAuctionProjectionV1 | undefined {
  try {
    const cachePolicy =
      cachePolicyValue === undefined ? undefined : parseCacheFetchPolicyV1(cachePolicyValue);
    if (cachePolicyValue !== undefined && !cachePolicy) return undefined;
    const record = ownDataObject(value, ['version', 'auction', 'bids']);
    if (!record || record.version !== 1) return undefined;
    const auction = parseAuctionDecisionSetV1(record.auction);
    const rawBids = ownDataArray(record.bids, MAX_AUCTION_RESULTS);
    if (!auction || !rawBids) return undefined;
    const bids: BrowserAuctionBidV1[] = [];
    const candidateIds = new Set<string>();
    const reservationIds = new Set<string>();
    for (let index = 0; index < rawBids.length; index += 1) {
      const raw = rawBids[index];
      const bid = parseBrowserBid(raw, cachePolicy);
      if (
        !bid ||
        candidateIds.has(bid.candidateId) ||
        reservationIds.has(bid.rendererReservationId)
      ) {
        return undefined;
      }
      candidateIds.add(bid.candidateId);
      reservationIds.add(bid.rendererReservationId);
      bids.push(bid);
    }

    let winnerIndex = 0;
    for (let index = 0; index < auction.results.length; index += 1) {
      const result = auction.results[index];
      if (!result || result.outcome !== 'winner') continue;
      const bid = bids[winnerIndex];
      if (bid?.candidateId !== result.candidateId || bid.slot !== result.slot) return undefined;
      winnerIndex += 1;
    }
    if (winnerIndex !== bids.length) return undefined;

    const projection: BrowserAuctionProjectionV1 = { version: 1, auction, bids };
    if (jsonUtf8ByteLength(projection) > MAX_BROWSER_AUCTION_PROJECTION_BYTES) {
      return undefined;
    }
    return projection;
  } catch {
    return undefined;
  }
}
