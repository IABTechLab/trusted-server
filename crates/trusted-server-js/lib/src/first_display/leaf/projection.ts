const HASH = /^[0-9a-f]{64}$/;
const AUCTION_ID = /^[A-Za-z0-9._:-]{1,128}$/;
const CANDIDATE_ID = /^[A-Za-z0-9_-]{12}$/;
const PROVIDER_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const RESERVATION_ID = /^r1_[A-Za-z0-9_-]{22}$/;
const TARGETING_KEY = /^[A-Za-z0-9_]{1,20}$/;
const MAX_SLOTS = 256;
const MAX_FORMATS = 64;
const MAX_TARGETING = 32;
const MAX_PROJECTION_BYTES = 8 * 1024 * 1024;
const MAX_ADM_BYTES = 512 * 1024;
const MAX_APS_ENVELOPE_BASE64_BYTES = 349_528;
const FAILURE_REASONS = new Set([
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

export type FirstDisplayAuctionProtocolId = 'aps' | 'gpt' | 'prebid';
export type FirstDisplayProjectedKind = 'no_bid' | 'failed' | 'gpt_adm' | 'aps';

export interface FirstDisplayBatchOutcomeV1 {
  readonly slotId: string;
  readonly kind: FirstDisplayProjectedKind;
}

export interface FirstDisplayProjectionSlotV1 {
  readonly slot: string;
  readonly gamUnitPath: string;
  readonly divId: string;
  readonly formats: readonly (readonly [number, number])[];
  readonly targeting: Readonly<Record<string, string>>;
}

export interface FirstDisplayAdmSourceV1 {
  readonly type: 'adm';
  readonly version: 1;
  readonly adm: string;
  readonly width: number;
  readonly height: number;
}

export interface FirstDisplayApsSourceV1 {
  readonly type: 'aps';
  readonly version: 1;
  readonly accountId: string;
  readonly bidId: string;
  readonly creativeId?: string;
  readonly tagType: 'iframe' | 'script';
  readonly creativeUrl: string;
  readonly aaxResponse: string;
  readonly width: number;
  readonly height: number;
}

export interface FirstDisplayProjectionBidV1 {
  readonly candidateId: string;
  readonly slot: string;
  readonly provider: string;
  readonly upstreamBidId: string;
  readonly cpm: number;
  readonly currency: 'USD';
  readonly targeting: Readonly<Record<string, string>>;
  readonly rendererReservationId: string;
  readonly renderSource: FirstDisplayAdmSourceV1 | FirstDisplayApsSourceV1;
}

export type FirstDisplayProjectionDecisionV1 =
  | Readonly<{ slot: string; outcome: 'winner'; candidateId: string }>
  | Readonly<{ slot: string; outcome: 'no_bid' }>
  | Readonly<{ slot: string; outcome: 'failed'; reason: string }>;

export interface FirstDisplayProjectionV1 {
  readonly version: 1;
  readonly auction: Readonly<{
    version: 1;
    auctionId: string;
    results: readonly FirstDisplayProjectionDecisionV1[];
  }>;
  readonly slots: readonly FirstDisplayProjectionSlotV1[];
  readonly bids: readonly FirstDisplayProjectionBidV1[];
}

export interface FirstDisplayBatchV1 {
  readonly version: 1;
  readonly projectionDigest: string;
  readonly requiredProtocols: readonly FirstDisplayAuctionProtocolId[];
  readonly outcomes: readonly FirstDisplayBatchOutcomeV1[];
  readonly projection: FirstDisplayProjectionV1;
}

/**
 * Admit the immutable same-script server projection without cloning it again.
 *
 * Rust owns the exhaustive projection grammar and the bootstrap recursively freezes
 * that generated data before this authenticated release artifact can receive it.
 */
export function acceptServerFirstDisplayBatchV1(
  candidate: unknown
): FirstDisplayBatchV1 | undefined {
  try {
    const envelope = candidate as Readonly<{
      version: unknown;
      projectionDigest: unknown;
      projection: FirstDisplayProjectionV1;
    }>;
    const projection = envelope.projection;
    const results = projection.auction.results;
    const { slots, bids } = projection;
    if (
      !Object.isFrozen(candidate) ||
      envelope.version !== 1 ||
      typeof envelope.projectionDigest !== 'string' ||
      !HASH.test(envelope.projectionDigest) ||
      !Object.isFrozen(projection) ||
      projection.version !== 1 ||
      projection.auction.version !== 1 ||
      !Object.isFrozen(results) ||
      results.length === 0 ||
      results.length > MAX_SLOTS ||
      !Object.isFrozen(slots) ||
      slots.length !== results.length ||
      !Object.isFrozen(bids) ||
      bids.length > results.length
    ) {
      return undefined;
    }
    const outcomes: FirstDisplayBatchOutcomeV1[] = [];
    let winner = 0;
    let aps = false;
    let prebid = false;
    for (let index = 0; index < results.length; index += 1) {
      const decision = results[index]!;
      const slot = slots[index]!;
      if (decision.slot !== slot.slot) return undefined;
      if (decision.outcome !== 'winner') {
        if (decision.outcome !== 'no_bid' && decision.outcome !== 'failed') return undefined;
        outcomes.push(Object.freeze({ slotId: decision.slot, kind: decision.outcome }));
        continue;
      }
      const bid = bids[winner++];
      if (
        !bid ||
        bid.candidateId !== decision.candidateId ||
        bid.slot !== decision.slot ||
        (bid.renderSource.type !== 'adm' && bid.renderSource.type !== 'aps')
      ) {
        return undefined;
      }
      aps ||= bid.renderSource.type === 'aps';
      prebid ||= bid.provider === 'prebid';
      outcomes.push(
        Object.freeze({
          slotId: decision.slot,
          kind: bid.renderSource.type === 'aps' ? 'aps' : 'gpt_adm',
        })
      );
    }
    if (winner !== bids.length) return undefined;
    return Object.freeze({
      version: 1,
      projectionDigest: envelope.projectionDigest,
      requiredProtocols: Object.freeze([
        ...(aps ? (['aps'] as const) : []),
        ...(winner ? (['gpt'] as const) : []),
        ...(prebid ? (['prebid'] as const) : []),
      ]),
      outcomes: Object.freeze(outcomes),
      projection,
    });
  } catch {
    return undefined;
  }
}

const textEncoder = new TextEncoder();

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

function exactArray(value: unknown, maximum: number): readonly unknown[] | undefined {
  try {
    if (
      !Array.isArray(value) ||
      Object.getPrototypeOf(value) !== Array.prototype ||
      !Object.isFrozen(value) ||
      value.length > maximum ||
      Reflect.ownKeys(value).length !== value.length + 1
    ) {
      return undefined;
    }
    const result: unknown[] = [];
    for (let index = 0; index < value.length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
      result.push(descriptor.value);
    }
    return result;
  } catch {
    return undefined;
  }
}

function validString(
  value: unknown,
  maximumBytes: number,
  options: Readonly<{ allowControls?: boolean; maximumScalars?: number }> = {}
): value is string {
  if (typeof value !== 'string' || value.length === 0) return false;
  let scalars = 0;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (options.allowControls !== true && (code <= 0x1f || code === 0x7f)) return false;
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return false;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return false;
    }
    scalars += 1;
    if (options.maximumScalars !== undefined && scalars > options.maximumScalars) return false;
  }
  return textEncoder.encode(value).byteLength <= maximumBytes;
}

function validDimension(value: unknown): value is number {
  return (
    typeof value === 'number' &&
    Number.isInteger(value) &&
    Number.isFinite(value) &&
    value >= 1 &&
    value <= 4096
  );
}

function snapshotTargeting(value: unknown): Readonly<Record<string, string>> | undefined {
  const record = exactRecord(value, Reflect.ownKeys(value as object) as string[]);
  if (!record) return undefined;
  const keys = Object.keys(record).sort();
  if (keys.length > MAX_TARGETING) return undefined;
  const result: Record<string, string> = {};
  for (const key of keys) {
    const entry = record[key];
    if (
      key === 'hb_adid' ||
      !TARGETING_KEY.test(key) ||
      !validString(entry, 160, { maximumScalars: 40 })
    ) {
      return undefined;
    }
    result[key] = entry;
  }
  return Object.freeze(result);
}

function snapshotFormats(value: unknown): readonly (readonly [number, number])[] | undefined {
  const formats = exactArray(value, MAX_FORMATS);
  if (!formats || formats.length === 0) return undefined;
  const result: Array<readonly [number, number]> = [];
  for (const candidate of formats) {
    const pair = exactArray(candidate, 2);
    if (!pair || pair.length !== 2 || !validDimension(pair[0]) || !validDimension(pair[1])) {
      return undefined;
    }
    result.push(Object.freeze([pair[0], pair[1]]));
  }
  return Object.freeze(result);
}

function snapshotSlot(value: unknown): FirstDisplayProjectionSlotV1 | undefined {
  const slot = exactRecord(value, ['slot', 'gamUnitPath', 'divId', 'formats', 'targeting']);
  const formats = snapshotFormats(slot?.formats);
  const targeting = snapshotTargeting(slot?.targeting);
  if (
    !slot ||
    !validString(slot.slot, 256) ||
    !validString(slot.gamUnitPath, 256) ||
    !validString(slot.divId, 256) ||
    !formats ||
    !targeting
  ) {
    return undefined;
  }
  return Object.freeze({
    slot: slot.slot,
    gamUnitPath: slot.gamUnitPath,
    divId: slot.divId,
    formats,
    targeting,
  });
}

function snapshotAdm(value: unknown): FirstDisplayAdmSourceV1 | undefined {
  const source = exactRecord(value, ['type', 'version', 'adm', 'width', 'height']);
  if (
    !source ||
    source.type !== 'adm' ||
    source.version !== 1 ||
    !validString(source.adm, MAX_ADM_BYTES, { allowControls: true }) ||
    !validDimension(source.width) ||
    !validDimension(source.height)
  ) {
    return undefined;
  }
  return Object.freeze({
    type: 'adm',
    version: 1,
    adm: source.adm,
    width: source.width,
    height: source.height,
  });
}

function snapshotAps(value: unknown): FirstDisplayApsSourceV1 | undefined {
  const raw = exactRecord(
    value,
    Object.prototype.hasOwnProperty.call(value, 'creativeId')
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
        ]
  );
  if (
    !raw ||
    raw.type !== 'aps' ||
    raw.version !== 1 ||
    !validString(raw.accountId, 1024, { allowControls: true }) ||
    !validString(raw.bidId, 64) ||
    (raw.creativeId !== undefined && !validString(raw.creativeId, 1024, { allowControls: true })) ||
    (raw.tagType !== 'iframe' && raw.tagType !== 'script') ||
    !validString(raw.creativeUrl, 4096) ||
    typeof raw.aaxResponse !== 'string' ||
    raw.aaxResponse.length > MAX_APS_ENVELOPE_BASE64_BYTES ||
    !validDimension(raw.width) ||
    !validDimension(raw.height)
  ) {
    return undefined;
  }
  return Object.freeze({
    type: 'aps',
    version: 1,
    accountId: raw.accountId,
    bidId: raw.bidId,
    ...(raw.creativeId === undefined ? {} : { creativeId: raw.creativeId }),
    tagType: raw.tagType,
    creativeUrl: raw.creativeUrl,
    aaxResponse: raw.aaxResponse,
    width: raw.width,
    height: raw.height,
  });
}

function snapshotBid(value: unknown): FirstDisplayProjectionBidV1 | undefined {
  const bid = exactRecord(value, [
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
  const targeting = snapshotTargeting(bid?.targeting);
  const sourceRecord = exactRecord(
    bid?.renderSource,
    Reflect.ownKeys((bid?.renderSource ?? {}) as object) as string[]
  );
  const renderSource =
    sourceRecord?.type === 'adm'
      ? snapshotAdm(bid?.renderSource)
      : sourceRecord?.type === 'aps'
        ? snapshotAps(bid?.renderSource)
        : undefined;
  if (
    !bid ||
    typeof bid.candidateId !== 'string' ||
    !CANDIDATE_ID.test(bid.candidateId) ||
    !validString(bid.slot, 256) ||
    typeof bid.provider !== 'string' ||
    !PROVIDER_ID.test(bid.provider) ||
    !validString(bid.upstreamBidId, 64) ||
    typeof bid.cpm !== 'number' ||
    !Number.isFinite(bid.cpm) ||
    bid.cpm < 0 ||
    bid.currency !== 'USD' ||
    !targeting ||
    typeof bid.rendererReservationId !== 'string' ||
    !RESERVATION_ID.test(bid.rendererReservationId) ||
    !renderSource
  ) {
    return undefined;
  }
  return Object.freeze({
    candidateId: bid.candidateId as string,
    slot: bid.slot,
    provider: bid.provider as string,
    upstreamBidId: bid.upstreamBidId,
    cpm: bid.cpm,
    currency: 'USD',
    targeting,
    rendererReservationId: bid.rendererReservationId,
    renderSource,
  });
}

function snapshotDecision(value: unknown): FirstDisplayProjectionDecisionV1 | undefined {
  const base = exactRecord(value, Reflect.ownKeys((value ?? {}) as object) as string[]);
  if (!base || !validString(base.slot, 256)) return undefined;
  if (base.outcome === 'winner') {
    const winner = exactRecord(value, ['slot', 'outcome', 'candidateId']);
    return winner && typeof winner.candidateId === 'string' && CANDIDATE_ID.test(winner.candidateId)
      ? Object.freeze({
          slot: winner.slot as string,
          outcome: 'winner',
          candidateId: winner.candidateId as string,
        })
      : undefined;
  }
  if (base.outcome === 'no_bid') {
    return exactRecord(value, ['slot', 'outcome'])
      ? Object.freeze({ slot: base.slot, outcome: 'no_bid' })
      : undefined;
  }
  if (base.outcome === 'failed') {
    const failed = exactRecord(value, ['slot', 'outcome', 'reason']);
    return failed && typeof failed.reason === 'string' && FAILURE_REASONS.has(failed.reason)
      ? Object.freeze({ slot: base.slot, outcome: 'failed', reason: failed.reason })
      : undefined;
  }
  return undefined;
}

/** Validate and freeze the sole server-projected batch admitted by the lean agent. */
export function snapshotFirstDisplayBatchV1(candidate: unknown): FirstDisplayBatchV1 | undefined {
  try {
    const envelope = exactRecord(candidate, ['version', 'projectionDigest', 'projection']);
    const rawProjection = exactRecord(envelope?.projection, [
      'version',
      'auction',
      'slots',
      'bids',
    ]);
    const rawAuction = exactRecord(rawProjection?.auction, ['version', 'auctionId', 'results']);
    const rawDecisions = exactArray(rawAuction?.results, MAX_SLOTS);
    const rawSlots = exactArray(rawProjection?.slots, MAX_SLOTS);
    const rawBids = exactArray(rawProjection?.bids, MAX_SLOTS);
    if (
      !envelope ||
      envelope.version !== 1 ||
      typeof envelope.projectionDigest !== 'string' ||
      !HASH.test(envelope.projectionDigest) ||
      !rawProjection ||
      rawProjection.version !== 1 ||
      !rawAuction ||
      rawAuction.version !== 1 ||
      typeof rawAuction.auctionId !== 'string' ||
      !AUCTION_ID.test(rawAuction.auctionId) ||
      !rawDecisions ||
      rawDecisions.length === 0 ||
      !rawSlots ||
      rawSlots.length !== rawDecisions.length ||
      !rawBids
    ) {
      return undefined;
    }

    const decisions: FirstDisplayProjectionDecisionV1[] = [];
    const slots: FirstDisplayProjectionSlotV1[] = [];
    const bids: FirstDisplayProjectionBidV1[] = [];
    const slotIds = new Set<string>();
    for (let index = 0; index < rawDecisions.length; index += 1) {
      const decision = snapshotDecision(rawDecisions[index]);
      const slot = snapshotSlot(rawSlots[index]);
      if (!decision || !slot || decision.slot !== slot.slot || slotIds.has(slot.slot)) {
        return undefined;
      }
      slotIds.add(slot.slot);
      decisions.push(decision);
      slots.push(slot);
    }

    const candidateIds = new Set<string>();
    const reservationIds = new Set<string>();
    for (const rawBid of rawBids) {
      const bid = snapshotBid(rawBid);
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

    const outcomes: FirstDisplayBatchOutcomeV1[] = [];
    let winnerIndex = 0;
    let aps = false;
    let prebid = false;
    for (const decision of decisions) {
      if (decision.outcome === 'winner') {
        const bid = bids[winnerIndex];
        if (!bid || bid.candidateId !== decision.candidateId || bid.slot !== decision.slot) {
          return undefined;
        }
        winnerIndex += 1;
        aps ||= bid.renderSource.type === 'aps';
        prebid ||= bid.provider === 'prebid';
        outcomes.push(
          Object.freeze({
            slotId: decision.slot,
            kind: bid.renderSource.type === 'aps' ? 'aps' : 'gpt_adm',
          })
        );
      } else {
        outcomes.push(
          Object.freeze({
            slotId: decision.slot,
            kind: decision.outcome,
          })
        );
      }
    }
    if (winnerIndex !== bids.length) return undefined;

    const projection: FirstDisplayProjectionV1 = Object.freeze({
      version: 1,
      auction: Object.freeze({
        version: 1,
        auctionId: rawAuction.auctionId,
        results: Object.freeze(decisions),
      }),
      slots: Object.freeze(slots),
      bids: Object.freeze(bids),
    });
    if (textEncoder.encode(JSON.stringify(projection)).byteLength > MAX_PROJECTION_BYTES) {
      return undefined;
    }
    return Object.freeze({
      version: 1,
      projectionDigest: envelope.projectionDigest,
      requiredProtocols: Object.freeze([
        ...(aps ? (['aps'] as const) : []),
        ...(winnerIndex > 0 ? (['gpt'] as const) : []),
        ...(prebid ? (['prebid'] as const) : []),
      ]),
      outcomes: Object.freeze(outcomes),
      projection,
    });
  } catch {
    return undefined;
  }
}
