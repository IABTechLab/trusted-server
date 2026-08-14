import type { FirstDisplaySliceActivationContext } from '../transaction';

export interface FirstDisplayPreparedTrustedBidV1 {
  readonly auctionId: string;
  readonly adUnitCode: string;
  readonly bid: Readonly<{
    readonly requestId: string;
    readonly adId: string;
    readonly cpm: number;
    readonly width: number;
    readonly height: number;
    readonly ad: '';
    readonly ttl: 300;
    readonly creativeId: string;
    readonly netRevenue: true;
    readonly currency: 'USD';
    readonly bidderCode: 'trustedServer';
    readonly meta: Readonly<{
      readonly advertiserDomains: readonly string[];
      readonly tsAuctionId: string;
      readonly tsBidId: string;
      readonly tsAdmHash?: string;
    }>;
  }>;
}

export interface FirstDisplayPrebidProtocolV1 {
  readonly version: 1;
  readonly id: 'prebid';
  readonly bidderCode: 'trustedServer';
  readonly maxPendingOperations: 64;
  readonly externalReadyMs: 10_000;
  readonly admissionLeaseMs: 10_000;
  readonly renderReservationMs: 900_000;
  readonly normalizeEidSource: (candidate: unknown) => string | undefined;
  readonly snapshotTrustedBid: (candidate: unknown) => FirstDisplayPreparedTrustedBidV1 | undefined;
}

interface PrebidInitialBindings {
  readonly observe: (name: 'protocol_version', value: number) => void;
  readonly register: (protocol: FirstDisplayPrebidProtocolV1) => () => void;
}

const textEncoder = new TextEncoder();
const RESERVATION = /^r1_[A-Za-z0-9_-]{22}$/;

function exactRecord(value: unknown, keys: readonly string[]): Record<string, unknown> | undefined {
  try {
    if (
      typeof value !== 'object' ||
      value === null ||
      Array.isArray(value) ||
      Object.getPrototypeOf(value) !== Object.prototype ||
      !Object.isFrozen(value)
    ) {
      return undefined;
    }
    const ownKeys = Reflect.ownKeys(value);
    if (
      ownKeys.length !== keys.length ||
      !ownKeys.every((key) => typeof key === 'string' && keys.includes(key))
    ) {
      return undefined;
    }
    const result: Record<string, unknown> = {};
    for (const key of keys) {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
      result[key] = descriptor.value;
    }
    return result;
  } catch {
    return undefined;
  }
}

function bindings(candidate: unknown): PrebidInitialBindings | undefined {
  const fields = exactRecord(candidate, ['observe', 'register']);
  return fields && typeof fields.observe === 'function' && typeof fields.register === 'function'
    ? (fields as unknown as PrebidInitialBindings)
    : undefined;
}

function validString(candidate: unknown, maximumBytes: number): candidate is string {
  if (
    typeof candidate !== 'string' ||
    candidate.length === 0 ||
    textEncoder.encode(candidate).byteLength > maximumBytes
  ) {
    return false;
  }
  for (let index = 0; index < candidate.length; index += 1) {
    const code = candidate.charCodeAt(index);
    if (code <= 0x1f || code === 0x7f) return false;
  }
  return true;
}

function frozenStrings(
  candidate: unknown,
  maximumLength: number,
  maximumBytes: number
): readonly string[] | undefined {
  if (
    !Array.isArray(candidate) ||
    !Object.isFrozen(candidate) ||
    candidate.length > maximumLength ||
    Reflect.ownKeys(candidate).length !== candidate.length + 1
  ) {
    return undefined;
  }
  const result: string[] = [];
  for (let index = 0; index < candidate.length; index += 1) {
    const descriptor = Object.getOwnPropertyDescriptor(candidate, String(index));
    if (
      !descriptor?.enumerable ||
      !('value' in descriptor) ||
      !validString(descriptor.value, maximumBytes)
    ) {
      return undefined;
    }
    result.push(descriptor.value);
  }
  return Object.freeze(result);
}

function normalizeEidSource(candidate: unknown): string | undefined {
  if (typeof candidate !== 'string') return undefined;
  const normalized = candidate.trim().toLowerCase();
  return validString(normalized, 256) ? normalized : undefined;
}

function snapshotTrustedBid(candidate: unknown): FirstDisplayPreparedTrustedBidV1 | undefined {
  try {
    const prepared = exactRecord(candidate, ['auctionId', 'adUnitCode', 'bid']);
    const bid = exactRecord(prepared?.bid, [
      'requestId',
      'adId',
      'cpm',
      'width',
      'height',
      'ad',
      'ttl',
      'creativeId',
      'netRevenue',
      'currency',
      'bidderCode',
      'meta',
    ]);
    if (
      !prepared ||
      !bid ||
      !validString(prepared.auctionId, 128) ||
      !validString(prepared.adUnitCode, 256) ||
      !validString(bid.requestId, 128) ||
      typeof bid.adId !== 'string' ||
      !RESERVATION.test(bid.adId) ||
      typeof bid.cpm !== 'number' ||
      !Number.isFinite(bid.cpm) ||
      bid.cpm < 0 ||
      typeof bid.width !== 'number' ||
      !Number.isInteger(bid.width) ||
      bid.width < 1 ||
      bid.width > 4096 ||
      typeof bid.height !== 'number' ||
      !Number.isInteger(bid.height) ||
      bid.height < 1 ||
      bid.height > 4096 ||
      bid.ad !== '' ||
      bid.ttl !== 300 ||
      !validString(bid.creativeId, 256) ||
      bid.netRevenue !== true ||
      bid.currency !== 'USD' ||
      bid.bidderCode !== 'trustedServer'
    ) {
      return undefined;
    }
    const hasAdmHash = Object.prototype.hasOwnProperty.call(bid.meta, 'tsAdmHash');
    const meta = exactRecord(
      bid.meta,
      hasAdmHash
        ? ['advertiserDomains', 'tsAuctionId', 'tsBidId', 'tsAdmHash']
        : ['advertiserDomains', 'tsAuctionId', 'tsBidId']
    );
    const advertiserDomains = frozenStrings(meta?.advertiserDomains, 16, 256);
    if (
      !meta ||
      !advertiserDomains ||
      meta.tsAuctionId !== prepared.auctionId ||
      !validString(meta.tsBidId, 256) ||
      (meta.tsAdmHash !== undefined && !validString(meta.tsAdmHash, 128))
    ) {
      return undefined;
    }
    const frozenMeta = Object.freeze({
      advertiserDomains,
      tsAuctionId: meta.tsAuctionId as string,
      tsBidId: meta.tsBidId,
      ...(meta.tsAdmHash === undefined ? {} : { tsAdmHash: meta.tsAdmHash }),
    });
    return Object.freeze({
      auctionId: prepared.auctionId,
      adUnitCode: prepared.adUnitCode,
      bid: Object.freeze({
        requestId: bid.requestId,
        adId: bid.adId,
        cpm: bid.cpm,
        width: bid.width,
        height: bid.height,
        ad: '',
        ttl: 300,
        creativeId: bid.creativeId,
        netRevenue: true,
        currency: 'USD',
        bidderCode: 'trustedServer',
        meta: frozenMeta,
      }),
    }) as FirstDisplayPreparedTrustedBidV1;
  } catch {
    return undefined;
  }
}

/** Register initial Prebid artifact, queue, EID, bidder, and reservation admission policy. */
export function installPrebidInitial(
  candidate: unknown,
  own: FirstDisplaySliceActivationContext['own']
): Readonly<{ version: 1; id: 'prebid' }> {
  const value = bindings(candidate);
  if (!value || typeof own !== 'function') throw new TypeError('invalid Prebid initial bindings');
  const protocol: FirstDisplayPrebidProtocolV1 = Object.freeze({
    version: 1,
    id: 'prebid',
    bidderCode: 'trustedServer',
    maxPendingOperations: 64,
    externalReadyMs: 10_000,
    admissionLeaseMs: 10_000,
    renderReservationMs: 900_000,
    normalizeEidSource,
    snapshotTrustedBid,
  });
  const release = value.register(protocol);
  if (typeof release !== 'function') throw new TypeError('invalid Prebid protocol disposer');
  own(release);
  value.observe('protocol_version', 1);
  return Object.freeze({ version: 1, id: 'prebid' });
}
