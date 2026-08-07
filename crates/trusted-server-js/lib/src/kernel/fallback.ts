import { parseCacheFetchPolicyV1 } from '../core/config';
import {
  parseBrowserAuctionProjectionV1,
  validBoundedString,
} from '../core/contracts/auction_projection';
import { log } from '../core/log';
import type { BootManifestV1 } from '../core/types';

import type { BootFailureReason } from './integration_registry';

const textEncoder = new TextEncoder();
const MAX_AUCTION_BODY_BYTES = 256 * 1024;
const MAX_JSON_ARRAY_ITEMS = Math.floor((MAX_AUCTION_BODY_BYTES - 1) / 2);
const SAFE_PROJECTION = {
  version: 1,
  auction: { version: 1, auctionId: 'fallback', results: [] },
  bids: [],
} as const;

export class RequestAdsInputError extends Error {
  public readonly code:
    | 'invalid_options'
    | 'invalid_slots'
    | 'empty_slots'
    | 'duplicate_slot'
    | 'invalid_timeout'
    | 'invalid_signal';

  public constructor(code: RequestAdsInputError['code']) {
    super(code);
    this.name = 'RequestAdsInputError';
    this.code = code;
  }
}

export type AdUnitRegistrationErrorCode =
  | 'invalid_units'
  | 'invalid_unit'
  | 'invalid_code'
  | 'duplicate_code'
  | 'slot_collision'
  | 'invalid_media_types'
  | 'invalid_dimensions'
  | 'dimensions_out_of_range'
  | 'invalid_bids'
  | 'invalid_bidder'
  | 'invalid_params'
  | 'request_body_too_large'
  | 'registry_capacity';

export class AdUnitRegistrationError extends Error {
  public readonly code: AdUnitRegistrationErrorCode;
  public readonly unitIndex?: number;

  public constructor(code: AdUnitRegistrationErrorCode, unitIndex?: number) {
    super(code);
    this.name = 'AdUnitRegistrationError';
    this.code = code;
    if (unitIndex !== undefined) this.unitIndex = unitIndex;
  }
}

export class TsjsUnavailableError extends Error {
  public readonly code = 'runtime_unavailable' as const;
  public readonly releaseId: string;
  public readonly reason: BootFailureReason;

  public constructor(releaseId: string, reason: BootFailureReason) {
    super('TSJS runtime is unavailable');
    this.name = 'TsjsUnavailableError';
    this.releaseId = releaseId;
    this.reason = reason;
  }
}

function ownDataRecord(value: unknown): Record<string, unknown> | undefined {
  try {
    if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
    const prototype = Object.getPrototypeOf(value) as unknown;
    if (prototype !== Object.prototype && prototype !== null) return undefined;
    const output: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    for (const key of Reflect.ownKeys(value)) {
      if (typeof key !== 'string') return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      output[key] = descriptor.value;
    }
    return output;
  } catch {
    return undefined;
  }
}

function exactKeys(record: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(record);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}

function snapshotOwnArray(value: unknown, maximum: number): readonly unknown[] | undefined {
  try {
    if (!Array.isArray(value) || value.length > maximum) {
      return undefined;
    }
    if (Object.getOwnPropertySymbols(value).length !== 0) return undefined;
    const names = Object.getOwnPropertyNames(value);
    if (names.length !== value.length + 1 || !names.includes('length')) return undefined;
    const copy: unknown[] = [];
    for (let index = 0; index < value.length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      copy.push(descriptor.value);
    }
    return copy;
  } catch {
    return undefined;
  }
}

function manifestMatches(candidate: unknown, expected: BootManifestV1): boolean {
  const record = ownDataRecord(candidate);
  if (!record || !exactKeys(record, ['version', 'releaseId', 'integrations'])) return false;
  if (record.version !== 1 || record.releaseId !== expected.releaseId) return false;
  const integrations = snapshotOwnArray(record.integrations, 16);
  if (!integrations || integrations.length !== expected.integrations.length) return false;
  return integrations.every((entry, index) => {
    const fields = ownDataRecord(entry);
    const accepted = expected.integrations[index];
    return Boolean(
      accepted &&
      fields &&
      exactKeys(fields, ['id', 'required']) &&
      fields.id === accepted.id &&
      fields.required === true
    );
  });
}

function deepFreeze<T>(value: T): Readonly<T> {
  if ((typeof value !== 'object' && typeof value !== 'function') || value === null) return value;
  for (const key of Reflect.ownKeys(value)) {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (descriptor && 'value' in descriptor) deepFreeze(descriptor.value);
  }
  return Object.freeze(value);
}

function readBootField(boot: unknown, key: string): unknown {
  const record = ownDataRecord(boot);
  return record?.[key];
}

function parseCachePolicy(candidate: unknown): ReturnType<typeof parseCacheFetchPolicyV1> {
  try {
    return parseCacheFetchPolicyV1(candidate);
  } catch {
    return undefined;
  }
}

/** Validate and freeze the complete boot snapshot used by a committed kernel. */
export function buildKernelBoot(
  releaseId: string,
  manifest: BootManifestV1,
  candidate: unknown
): Readonly<object> | undefined {
  const record = ownDataRecord(candidate);
  if (!record) return undefined;
  const transportKeys = ['auctionProjection', 'creative', 'diagnostics'];
  if (Object.prototype.hasOwnProperty.call(record, 'cachePolicy'))
    transportKeys.push('cachePolicy');
  const completeKeys = ['abi', 'releaseId', 'manifest', ...transportKeys];
  if (!exactKeys(record, transportKeys) && !exactKeys(record, completeKeys)) return undefined;
  if (
    completeKeys.length === Object.keys(record).length &&
    (record.abi !== 1 ||
      record.releaseId !== releaseId ||
      !manifestMatches(record.manifest, manifest))
  ) {
    return undefined;
  }
  const cachePolicy =
    record.cachePolicy === undefined ? undefined : parseCachePolicy(record.cachePolicy);
  if (record.cachePolicy !== undefined && !cachePolicy) return undefined;
  const auctionProjection = parseBrowserAuctionProjectionV1(record.auctionProjection, cachePolicy);
  const creative = ownDataRecord(record.creative);
  const diagnostics = ownDataRecord(record.diagnostics);
  const gptDiagnostics = ownDataRecord(diagnostics?.gpt);
  if (
    !auctionProjection ||
    !creative ||
    !exactKeys(creative, ['version', 'enabled', 'clickGuard', 'renderGuard']) ||
    creative.version !== 1 ||
    typeof creative.enabled !== 'boolean' ||
    typeof creative.clickGuard !== 'boolean' ||
    typeof creative.renderGuard !== 'boolean' ||
    !diagnostics ||
    !exactKeys(diagnostics, ['version', 'renderTraceOverlay', 'gpt']) ||
    diagnostics.version !== 1 ||
    typeof diagnostics.renderTraceOverlay !== 'boolean' ||
    !gptDiagnostics ||
    !exactKeys(gptDiagnostics, ['active']) ||
    typeof gptDiagnostics.active !== 'boolean'
  ) {
    return undefined;
  }
  const diagnosticsModule = manifest.integrations.filter(({ id }) => id === 'gpt_diagnostics');
  if (
    (gptDiagnostics.active && diagnosticsModule.length !== 1) ||
    (!gptDiagnostics.active && diagnosticsModule.length !== 0)
  ) {
    return undefined;
  }
  return deepFreeze({
    abi: 1,
    releaseId,
    manifest,
    auctionProjection,
    ...(cachePolicy ? { cachePolicy } : {}),
    creative: {
      version: 1,
      enabled: creative.enabled,
      clickGuard: creative.clickGuard,
      renderGuard: creative.renderGuard,
    },
    diagnostics: {
      version: 1,
      renderTraceOverlay: diagnostics.renderTraceOverlay,
      gpt: { active: gptDiagnostics.active },
    },
  });
}

/** Build the immutable boot snapshot shared by every terminal fallback. */
export function buildFallbackBoot(releaseId: string, candidate: unknown): Readonly<object> {
  const cacheCandidate = readBootField(candidate, 'cachePolicy');
  const cachePolicy = cacheCandidate === undefined ? undefined : parseCachePolicy(cacheCandidate);
  const projection =
    parseBrowserAuctionProjectionV1(readBootField(candidate, 'auctionProjection'), cachePolicy) ??
    SAFE_PROJECTION;
  return deepFreeze({
    abi: 1,
    releaseId,
    manifest: { version: 1, releaseId, integrations: [] },
    auctionProjection: projection,
    ...(cachePolicy ? { cachePolicy } : {}),
    creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
    diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
  });
}

function validSlotId(value: unknown): value is string {
  return validBoundedString(value, 256);
}

function readAborted(signal: unknown): boolean | undefined {
  try {
    const getter = Object.getOwnPropertyDescriptor(AbortSignal.prototype, 'aborted')?.get;
    return getter?.call(signal) as boolean | undefined;
  } catch {
    return undefined;
  }
}

function validateRequestOptions(value: unknown): {
  readonly slots: readonly string[] | undefined;
  readonly aborted: boolean;
} {
  if (value === undefined) return { slots: undefined, aborted: false };
  const options = ownDataRecord(value);
  if (
    !options ||
    !Object.keys(options).every((key) => ['slots', 'timeoutMs', 'signal'].includes(key))
  ) {
    throw new RequestAdsInputError('invalid_options');
  }
  let slots: readonly string[] | undefined;
  if (Object.prototype.hasOwnProperty.call(options, 'slots')) {
    const candidateSlots = snapshotOwnArray(options.slots, 256);
    if (!candidateSlots) {
      throw new RequestAdsInputError('invalid_slots');
    }
    if (candidateSlots.length === 0) throw new RequestAdsInputError('empty_slots');
    const seen = new Set<string>();
    const copy: string[] = [];
    for (const slot of candidateSlots) {
      if (!validSlotId(slot)) throw new RequestAdsInputError('invalid_slots');
      if (seen.has(slot)) throw new RequestAdsInputError('duplicate_slot');
      seen.add(slot);
      copy.push(slot);
    }
    slots = Object.freeze(copy);
  }
  if (
    Object.prototype.hasOwnProperty.call(options, 'timeoutMs') &&
    (!Number.isInteger(options.timeoutMs) ||
      (options.timeoutMs as number) < 100 ||
      (options.timeoutMs as number) > 30_000)
  ) {
    throw new RequestAdsInputError('invalid_timeout');
  }
  let aborted = false;
  if (Object.prototype.hasOwnProperty.call(options, 'signal')) {
    const candidate = readAborted(options.signal);
    if (candidate === undefined) throw new RequestAdsInputError('invalid_signal');
    aborted = candidate;
  }
  return { slots, aborted };
}

interface JsonMeasurement {
  readonly bytes: number;
}

interface JsonMeasurementContext {
  readonly memo: WeakMap<object, JsonMeasurement>;
  readonly snapshots: WeakMap<object, JsonNode | typeof JSON_TOO_LARGE | null>;
}

interface JsonNode {
  readonly entries: readonly JsonEntry[];
}

interface JsonEntry {
  readonly prefixBytes: number;
  readonly value: unknown;
}

interface JsonFrame {
  readonly object: object;
  readonly node: JsonNode;
  bytes: number;
  index: number;
}

const JSON_TOO_LARGE = Symbol('json_too_large');
const TOO_LARGE_MEASUREMENT = Object.freeze({ bytes: MAX_AUCTION_BODY_BYTES + 1 });

function boundedByteSum(left: number, right: number): number {
  return Math.min(MAX_AUCTION_BODY_BYTES + 1, left + right);
}

function primitiveJsonBytes(value: unknown): number | undefined {
  if (value === null) return 4;
  if (typeof value === 'boolean') return value ? 4 : 5;
  if (typeof value === 'string') return textEncoder.encode(JSON.stringify(value)).length;
  if (typeof value === 'number' && Number.isFinite(value)) return String(value).length;
  return undefined;
}

function snapshotJsonNode(
  value: unknown,
  context: JsonMeasurementContext,
  recordSnapshot?: Record<string, unknown>
): JsonNode | typeof JSON_TOO_LARGE | undefined {
  if (typeof value !== 'object' || value === null) return undefined;
  if (context.snapshots.has(value)) {
    return context.snapshots.get(value) ?? undefined;
  }
  let node: JsonNode | typeof JSON_TOO_LARGE | undefined;
  try {
    let entries: JsonEntry[];
    if (recordSnapshot) {
      entries = Object.keys(recordSnapshot).map((key, index) => ({
        prefixBytes: (index === 0 ? 0 : 1) + textEncoder.encode(JSON.stringify(key)).length + 1,
        value: recordSnapshot[key],
      }));
    } else if (Array.isArray(value)) {
      if (value.length > MAX_JSON_ARRAY_ITEMS) {
        node = JSON_TOO_LARGE;
        return node;
      }
      const values = snapshotOwnArray(value, MAX_JSON_ARRAY_ITEMS);
      if (!values) return undefined;
      entries = values.map((entry, index) => ({
        prefixBytes: index === 0 ? 0 : 1,
        value: entry,
      }));
    } else {
      const record = ownDataRecord(value);
      if (!record) return undefined;
      entries = Object.keys(record).map((key, index) => ({
        prefixBytes: (index === 0 ? 0 : 1) + textEncoder.encode(JSON.stringify(key)).length + 1,
        value: record[key],
      }));
    }
    node = Object.freeze({ entries: Object.freeze(entries) });
    return node;
  } catch {
    return undefined;
  } finally {
    context.snapshots.set(value, node ?? null);
  }
}

function measureJsonData(
  value: unknown,
  context: JsonMeasurementContext,
  recordSnapshot?: Record<string, unknown>
): JsonMeasurement | undefined {
  const primitiveBytes = primitiveJsonBytes(value);
  if (primitiveBytes !== undefined) return { bytes: primitiveBytes };
  if (typeof value !== 'object' || value === null) return undefined;
  const cached = context.memo.get(value);
  if (cached) return cached;
  const root = snapshotJsonNode(value, context, recordSnapshot);
  if (root === JSON_TOO_LARGE) return TOO_LARGE_MEASUREMENT;
  if (!root) return undefined;

  const active = new Set<object>([value]);
  const stack: JsonFrame[] = [{ object: value, node: root, bytes: 2, index: 0 }];
  while (stack.length > 0) {
    const frame = stack[stack.length - 1];
    if (!frame) return undefined;
    if (frame.index >= frame.node.entries.length) {
      const measurement = Object.freeze({ bytes: frame.bytes });
      context.memo.set(frame.object, measurement);
      active.delete(frame.object);
      stack.pop();
      const parent = stack[stack.length - 1];
      if (!parent) return measurement;
      parent.bytes = boundedByteSum(parent.bytes, measurement.bytes);
      if (parent.bytes > MAX_AUCTION_BODY_BYTES) return TOO_LARGE_MEASUREMENT;
      continue;
    }

    const entry = frame.node.entries[frame.index];
    frame.index += 1;
    if (!entry) return undefined;
    frame.bytes = boundedByteSum(frame.bytes, entry.prefixBytes);
    if (frame.bytes > MAX_AUCTION_BODY_BYTES) return TOO_LARGE_MEASUREMENT;
    const childBytes = primitiveJsonBytes(entry.value);
    if (childBytes !== undefined) {
      frame.bytes = boundedByteSum(frame.bytes, childBytes);
      if (frame.bytes > MAX_AUCTION_BODY_BYTES) return TOO_LARGE_MEASUREMENT;
      continue;
    }
    if (typeof entry.value !== 'object' || entry.value === null || active.has(entry.value)) {
      return undefined;
    }
    const childMeasurement = context.memo.get(entry.value);
    if (childMeasurement) {
      frame.bytes = boundedByteSum(frame.bytes, childMeasurement.bytes);
      if (frame.bytes > MAX_AUCTION_BODY_BYTES) return TOO_LARGE_MEASUREMENT;
      continue;
    }
    const childNode = snapshotJsonNode(entry.value, context);
    if (childNode === JSON_TOO_LARGE) return TOO_LARGE_MEASUREMENT;
    if (!childNode) return undefined;
    active.add(entry.value);
    stack.push({ object: entry.value, node: childNode, bytes: 2, index: 0 });
  }
  return undefined;
}

function measureJsonRecord(
  value: unknown,
  context: JsonMeasurementContext
): JsonMeasurement | undefined {
  try {
    if (Array.isArray(value)) return undefined;
  } catch {
    return undefined;
  }
  const record = ownDataRecord(value);
  return record ? measureJsonData(value, context, record) : undefined;
}

function validateProgrammaticUnits(value: unknown, knownSlots: ReadonlySet<string>): void {
  let units: readonly unknown[] | undefined;
  try {
    units = Array.isArray(value) ? snapshotOwnArray(value, 256) : [value];
  } catch {
    throw new AdUnitRegistrationError('invalid_units');
  }
  if (!units) throw new AdUnitRegistrationError('invalid_units');
  if (units.length === 0 || units.length > 256) throw new AdUnitRegistrationError('invalid_units');
  const seen = new Set<string>();
  const measurementContext: JsonMeasurementContext = {
    memo: new WeakMap(),
    snapshots: new WeakMap(),
  };
  for (let index = 0; index < units.length; index += 1) {
    const unit = ownDataRecord(units[index]);
    if (
      !unit ||
      (!exactKeys(unit, ['code', 'mediaTypes']) && !exactKeys(unit, ['code', 'mediaTypes', 'bids']))
    ) {
      throw new AdUnitRegistrationError('invalid_unit', index);
    }
    if (!validSlotId(unit.code)) throw new AdUnitRegistrationError('invalid_code', index);
    if (seen.has(unit.code)) throw new AdUnitRegistrationError('duplicate_code', index);
    if (knownSlots.has(unit.code)) throw new AdUnitRegistrationError('slot_collision', index);
    seen.add(unit.code);
    const mediaTypes = ownDataRecord(unit.mediaTypes);
    const banner = ownDataRecord(mediaTypes?.banner);
    if (
      !mediaTypes ||
      !exactKeys(mediaTypes, ['banner']) ||
      !banner ||
      !exactKeys(banner, ['sizes'])
    ) {
      throw new AdUnitRegistrationError('invalid_media_types', index);
    }
    const sizes = snapshotOwnArray(banner.sizes, MAX_JSON_ARRAY_ITEMS);
    if (!sizes || sizes.length === 0) {
      throw new AdUnitRegistrationError('invalid_media_types', index);
    }
    for (const size of sizes) {
      const dimensions = snapshotOwnArray(size, 2);
      if (
        !dimensions ||
        dimensions.length !== 2 ||
        dimensions.some(
          (dimension) =>
            typeof dimension !== 'number' ||
            !Number.isFinite(dimension) ||
            !Number.isInteger(dimension) ||
            dimension <= 0
        )
      ) {
        throw new AdUnitRegistrationError('invalid_dimensions', index);
      }
      if (dimensions.some((dimension) => (dimension as number) > 4096)) {
        throw new AdUnitRegistrationError('dimensions_out_of_range', index);
      }
    }
    if (unit.bids !== undefined) {
      const bids = snapshotOwnArray(unit.bids, MAX_JSON_ARRAY_ITEMS);
      if (!bids) throw new AdUnitRegistrationError('invalid_bids', index);
      for (const rawBid of bids) {
        const bid = ownDataRecord(rawBid);
        if (!bid || (!exactKeys(bid, ['bidder']) && !exactKeys(bid, ['bidder', 'params']))) {
          throw new AdUnitRegistrationError('invalid_bids', index);
        }
        if (
          typeof bid.bidder !== 'string' ||
          textEncoder.encode(bid.bidder).length > 64 ||
          bid.bidder.length === 0
        ) {
          throw new AdUnitRegistrationError('invalid_bidder', index);
        }
        if (bid.params !== undefined) {
          const measured = measureJsonRecord(bid.params, measurementContext);
          if (!measured) {
            throw new AdUnitRegistrationError('invalid_params', index);
          }
        }
      }
    }
  }
  const measured = measureJsonData(units, measurementContext);
  if (!measured) {
    throw new AdUnitRegistrationError('invalid_params');
  }
  if (measured.bytes > MAX_AUCTION_BODY_BYTES) {
    throw new AdUnitRegistrationError('request_body_too_large');
  }
  if (knownSlots.size + units.length > 256) {
    throw new AdUnitRegistrationError('registry_capacity');
  }
}

const LOG_LEVELS = Object.freeze({
  silent: true,
  error: true,
  warn: true,
  info: true,
  debug: true,
});

function observeLog(callback: () => void): void {
  try {
    callback();
  } catch {
    // The public logger is observation only.
  }
}

export const publicLog = Object.freeze({
  setLevel: (level: Parameters<typeof log.setLevel>[0]) => {
    if (!Object.prototype.hasOwnProperty.call(LOG_LEVELS, level)) {
      throw new TypeError('Invalid TSJS log level');
    }
    log.setLevel(level);
  },
  getLevel: () => log.getLevel(),
  error: (...values: readonly unknown[]) => observeLog(() => log.error(...values)),
  warn: (...values: readonly unknown[]) => observeLog(() => log.warn(...values)),
  info: (...values: readonly unknown[]) => observeLog(() => log.info(...values)),
  debug: (...values: readonly unknown[]) => observeLog(() => log.debug(...values)),
});

export interface FallbackFieldsOptions {
  readonly releaseId: string;
  readonly reason: BootFailureReason;
  readonly boot: unknown;
}

/** Construct the complete, non-rendering public shell without runtime services. */
export function createFallbackFields(
  options: FallbackFieldsOptions
): Readonly<Record<string, unknown>> {
  const boot = buildFallbackBoot(options.releaseId, options.boot);
  const projection = boot as {
    readonly auctionProjection: {
      readonly auction: { readonly results: readonly { slot: string }[] };
    };
  };
  const knownSlots = Object.freeze(
    projection.auctionProjection.auction.results.map(({ slot }) => slot)
  );
  const known = new Set(knownSlots);
  const fields: Record<string, unknown> = {};
  Object.defineProperties(fields, {
    version: { enumerable: true, value: '1.0.0' },
    releaseId: { enumerable: true, value: options.releaseId },
    boot: { enumerable: true, value: boot },
    log: { enumerable: true, value: publicLog },
    _registerIntegration: { enumerable: true, value: () => false },
    addAdUnits: {
      enumerable: true,
      value: (units: unknown) => {
        validateProgrammaticUnits(units, known);
        throw new TsjsUnavailableError(options.releaseId, options.reason);
      },
    },
    requestAds: {
      enumerable: true,
      value: async (requestOptions?: unknown) => {
        const validated = validateRequestOptions(requestOptions);
        const selected = validated.slots ?? knownSlots;
        return deepFreeze({
          slots: selected.map((slot) =>
            known.has(slot)
              ? validated.aborted
                ? { slot, path: 'primary', outcome: 'cancelled', reason: 'caller_aborted' }
                : { slot, path: 'primary', outcome: 'failed', reason: options.reason }
              : { slot, path: 'primary', outcome: 'failed', reason: 'slot_unresolved' }
          ),
        });
      },
    },
    _internal: {
      enumerable: false,
      value: Object.freeze({
        state: 'fallback',
        releaseId: options.releaseId,
        reason: options.reason,
      }),
    },
  });
  return Object.freeze(fields);
}
