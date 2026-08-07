// Programmatic ad-unit validation plus the legacy registry retained until Task 19.
import type { AdUnit, AddAdUnitsResult, ProgrammaticAdUnit, Size } from './types';
import { validBoundedString } from './contracts/auction_projection';
import { log } from './log';
import { toArray } from './util';

const MAX_AUCTION_BODY_BYTES = 256 * 1024;
const MAX_PROGRAMMATIC_UNITS = 256;
const MAX_ACTIVE_SLOT_RECORDS = 256;
const MAX_JSON_STRUCTURE_ENTRIES = Math.floor((MAX_AUCTION_BODY_BYTES - 1) / 2);
const textEncoder = new TextEncoder();

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

interface JsonContainerSnapshot {
  readonly array: boolean;
  readonly entries: readonly Readonly<{ key: string; value: unknown }>[];
}

interface JsonCloneFrame {
  readonly output: Record<string, unknown> | unknown[];
  readonly snapshot: JsonContainerSnapshot;
  readonly source: object;
  index: number;
}

interface JsonMeasureFrame {
  readonly array: boolean;
  readonly entries: readonly Readonly<{ key: string; value: unknown }>[];
  readonly source: object;
  bytes: number;
  index: number;
}

function ownDataRecord(value: unknown): Record<string, unknown> | undefined {
  try {
    if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
    const prototype = Object.getPrototypeOf(value) as unknown;
    if (prototype !== Object.prototype && prototype !== null) return undefined;
    if (Object.getOwnPropertySymbols(value).length !== 0) return undefined;
    const output: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    for (const key of Object.getOwnPropertyNames(value)) {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      Object.defineProperty(output, key, {
        configurable: true,
        enumerable: true,
        value: descriptor.value,
        writable: true,
      });
    }
    return output;
  } catch {
    return undefined;
  }
}

function ownDataArray(value: unknown, maximum: number): readonly unknown[] | undefined {
  try {
    if (
      !Array.isArray(value) ||
      Object.getPrototypeOf(value) !== Array.prototype ||
      Object.getOwnPropertySymbols(value).length !== 0
    ) {
      return undefined;
    }
    const length = Object.getOwnPropertyDescriptor(value, 'length');
    if (
      !length ||
      !('value' in length) ||
      !Number.isSafeInteger(length.value) ||
      length.value < 0 ||
      length.value > maximum ||
      Object.getOwnPropertyNames(value).length !== length.value + 1
    ) {
      return undefined;
    }
    const output: unknown[] = [];
    for (let index = 0; index < length.value; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      output[index] = descriptor.value;
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

function jsonPrimitive(value: unknown): null | boolean | number | string | undefined {
  if (value === null || typeof value === 'boolean' || typeof value === 'string') return value;
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function snapshotJsonContainer(value: object): JsonContainerSnapshot | undefined {
  const array = Array.isArray(value);
  const values = array ? ownDataArray(value, MAX_JSON_STRUCTURE_ENTRIES) : undefined;
  if (array && !values) return undefined;
  const record = array ? undefined : ownDataRecord(value);
  if (!array && !record) return undefined;
  const entries = array
    ? values!.map((entry, index) => Object.freeze({ key: String(index), value: entry }))
    : Object.keys(record!).map((key) => Object.freeze({ key, value: record![key] }));
  return Object.freeze({ array, entries: Object.freeze(entries) });
}

/** Copy JSON data without invoking accessors or retaining publisher-owned objects. */
function copyJsonRecord(value: unknown): Readonly<Record<string, unknown>> | undefined {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
  const rootSnapshot = snapshotJsonContainer(value);
  if (!rootSnapshot || rootSnapshot.array) return undefined;
  const root: Record<string, unknown> = {};
  const active = new Set<object>([value]);
  const completed = new WeakMap<object, Record<string, unknown> | unknown[]>();
  const stack: JsonCloneFrame[] = [
    { index: 0, output: root, snapshot: rootSnapshot, source: value },
  ];
  let structureEntries = 1;
  try {
    while (stack.length > 0) {
      const frame = stack[stack.length - 1];
      if (!frame) return undefined;
      if (frame.index >= frame.snapshot.entries.length) {
        Object.freeze(frame.output);
        completed.set(frame.source, frame.output);
        active.delete(frame.source);
        stack.pop();
        continue;
      }
      const entry = frame.snapshot.entries[frame.index];
      frame.index += 1;
      if (!entry || ++structureEntries > MAX_JSON_STRUCTURE_ENTRIES) return undefined;
      const primitive = jsonPrimitive(entry.value);
      if (primitive !== undefined || entry.value === null) {
        Object.defineProperty(frame.output, entry.key, {
          configurable: true,
          enumerable: true,
          value: primitive,
          writable: true,
        });
        continue;
      }
      if (typeof entry.value !== 'object' || entry.value === null || active.has(entry.value)) {
        return undefined;
      }
      const completedChild = completed.get(entry.value);
      if (completedChild) {
        Object.defineProperty(frame.output, entry.key, {
          configurable: true,
          enumerable: true,
          value: completedChild,
          writable: true,
        });
        continue;
      }
      const childSnapshot = snapshotJsonContainer(entry.value);
      if (!childSnapshot) return undefined;
      const child: Record<string, unknown> | unknown[] = childSnapshot.array ? [] : {};
      Object.defineProperty(frame.output, entry.key, {
        configurable: true,
        enumerable: true,
        value: child,
        writable: true,
      });
      active.add(entry.value);
      stack.push({ index: 0, output: child, snapshot: childSnapshot, source: entry.value });
    }
    return Object.freeze(root);
  } catch {
    return undefined;
  }
}

function encodedJsonStringBytes(value: string): number {
  let bytes = 2;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code === 0x22 || code === 0x5c) bytes += 2;
    else if (code <= 0x1f) {
      bytes +=
        code === 0x08 || code === 0x09 || code === 0x0a || code === 0x0c || code === 0x0d ? 2 : 6;
    } else if (code <= 0x7f) bytes += 1;
    else if (code <= 0x7ff) bytes += 2;
    else if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (next >= 0xdc00 && next <= 0xdfff) {
        bytes += 4;
        index += 1;
      } else bytes += 6;
    } else if (code >= 0xdc00 && code <= 0xdfff) bytes += 6;
    else bytes += 3;
    if (bytes > MAX_AUCTION_BODY_BYTES) return bytes;
  }
  return bytes;
}

function primitiveJsonBytes(value: unknown): number | undefined {
  if (value === null) return 4;
  if (typeof value === 'boolean') return value ? 4 : 5;
  if (typeof value === 'string') return encodedJsonStringBytes(value);
  if (typeof value === 'number' && Number.isFinite(value)) return String(value).length;
  return undefined;
}

function boundedBytes(left: number, right: number): number {
  return left > MAX_AUCTION_BODY_BYTES - right ? MAX_AUCTION_BODY_BYTES + 1 : left + right;
}

/** Exact JSON byte measurement that never consults `toJSON` or publisher prototypes. */
function measureJsonBytes(value: unknown): number | undefined {
  const primitive = primitiveJsonBytes(value);
  if (primitive !== undefined) return primitive;
  if (typeof value !== 'object' || value === null) return undefined;
  const root = snapshotJsonContainer(value);
  if (!root) return undefined;
  const memo = new WeakMap<object, number>();
  const active = new Set<object>([value]);
  const stack: JsonMeasureFrame[] = [
    { array: root.array, bytes: 2, entries: root.entries, index: 0, source: value },
  ];
  while (stack.length > 0) {
    const frame = stack[stack.length - 1];
    if (!frame) return undefined;
    if (frame.index >= frame.entries.length) {
      memo.set(frame.source, frame.bytes);
      active.delete(frame.source);
      stack.pop();
      const parent = stack[stack.length - 1];
      if (!parent) return frame.bytes;
      parent.bytes = boundedBytes(parent.bytes, frame.bytes);
      if (parent.bytes > MAX_AUCTION_BODY_BYTES) return parent.bytes;
      continue;
    }
    const entry = frame.entries[frame.index];
    const entryIndex = frame.index;
    frame.index += 1;
    if (!entry) return undefined;
    const prefix =
      (entryIndex === 0 ? 0 : 1) + (frame.array ? 0 : encodedJsonStringBytes(entry.key) + 1);
    frame.bytes = boundedBytes(frame.bytes, prefix);
    if (frame.bytes > MAX_AUCTION_BODY_BYTES) return frame.bytes;
    const childPrimitive = primitiveJsonBytes(entry.value);
    if (childPrimitive !== undefined) {
      frame.bytes = boundedBytes(frame.bytes, childPrimitive);
      if (frame.bytes > MAX_AUCTION_BODY_BYTES) return frame.bytes;
      continue;
    }
    if (typeof entry.value !== 'object' || entry.value === null || active.has(entry.value)) {
      return undefined;
    }
    const completed = memo.get(entry.value);
    if (completed !== undefined) {
      frame.bytes = boundedBytes(frame.bytes, completed);
      if (frame.bytes > MAX_AUCTION_BODY_BYTES) return frame.bytes;
      continue;
    }
    const child = snapshotJsonContainer(entry.value);
    if (!child) return undefined;
    active.add(entry.value);
    stack.push({
      array: child.array,
      bytes: 2,
      entries: child.entries,
      index: 0,
      source: entry.value,
    });
  }
  return undefined;
}

function snapshotKnownSlots(knownSlots: ReadonlySet<string>): ReadonlySet<string> {
  try {
    return new Set(knownSlots);
  } catch {
    throw new AdUnitRegistrationError('slot_collision');
  }
}

/**
 * Validate and detach one complete public registration call before slot mutation.
 *
 * The returned graph is recursively frozen and safe to serialize later without
 * reading publisher accessors again.
 */
export function prepareProgrammaticAdUnits(
  value: unknown,
  knownSlots: ReadonlySet<string>
): readonly ProgrammaticAdUnit[] {
  let units: readonly unknown[] | undefined;
  try {
    units = Array.isArray(value) ? ownDataArray(value, MAX_PROGRAMMATIC_UNITS) : [value];
  } catch {
    units = undefined;
  }
  if (!units || units.length === 0 || units.length > MAX_PROGRAMMATIC_UNITS) {
    throw new AdUnitRegistrationError('invalid_units');
  }

  const occupied = snapshotKnownSlots(knownSlots);
  const seen = new Set<string>();
  const prepared: ProgrammaticAdUnit[] = [];
  for (let index = 0; index < units.length; index += 1) {
    const unit = ownDataRecord(units[index]);
    if (
      !unit ||
      (!exactKeys(unit, ['code', 'mediaTypes']) && !exactKeys(unit, ['code', 'mediaTypes', 'bids']))
    ) {
      throw new AdUnitRegistrationError('invalid_unit', index);
    }
    if (!validBoundedString(unit.code, 256)) {
      throw new AdUnitRegistrationError('invalid_code', index);
    }
    if (seen.has(unit.code)) throw new AdUnitRegistrationError('duplicate_code', index);
    if (occupied.has(unit.code)) throw new AdUnitRegistrationError('slot_collision', index);
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
    const rawSizes = ownDataArray(banner.sizes, MAX_JSON_STRUCTURE_ENTRIES);
    if (!rawSizes || rawSizes.length === 0) {
      throw new AdUnitRegistrationError('invalid_media_types', index);
    }
    const sizes: Array<readonly [number, number]> = [];
    for (const rawSize of rawSizes) {
      const dimensions = ownDataArray(rawSize, 2);
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
      if (dimensions.some((dimension) => (dimension as number) > 4_096)) {
        throw new AdUnitRegistrationError('dimensions_out_of_range', index);
      }
      sizes.push(Object.freeze([dimensions[0] as number, dimensions[1] as number]));
    }

    let bids: ProgrammaticAdUnit['bids'];
    if (unit.bids !== undefined) {
      const rawBids = ownDataArray(unit.bids, MAX_JSON_STRUCTURE_ENTRIES);
      if (!rawBids) throw new AdUnitRegistrationError('invalid_bids', index);
      const copiedBids: Array<NonNullable<ProgrammaticAdUnit['bids']>[number]> = [];
      for (const rawBid of rawBids) {
        const bid = ownDataRecord(rawBid);
        if (!bid || (!exactKeys(bid, ['bidder']) && !exactKeys(bid, ['bidder', 'params']))) {
          throw new AdUnitRegistrationError('invalid_bids', index);
        }
        if (
          typeof bid.bidder !== 'string' ||
          bid.bidder.length === 0 ||
          textEncoder.encode(bid.bidder).byteLength > 64
        ) {
          throw new AdUnitRegistrationError('invalid_bidder', index);
        }
        let params: Readonly<Record<string, unknown>> | undefined;
        if (bid.params !== undefined) {
          params = copyJsonRecord(bid.params);
          if (!params) throw new AdUnitRegistrationError('invalid_params', index);
        }
        copiedBids.push(
          Object.freeze({ bidder: bid.bidder, ...(params === undefined ? {} : { params }) })
        );
      }
      bids = Object.freeze(copiedBids);
    }

    prepared.push(
      Object.freeze({
        code: unit.code,
        mediaTypes: Object.freeze({
          banner: Object.freeze({ sizes: Object.freeze(sizes) }),
        }),
        ...(bids === undefined ? {} : { bids }),
      })
    );
  }

  const unitsBytes = measureJsonBytes(prepared);
  if (unitsBytes === undefined) throw new AdUnitRegistrationError('invalid_params');
  // `{"adUnits":` + encoded array + `}`.
  if (boundedBytes(12, unitsBytes) > MAX_AUCTION_BODY_BYTES) {
    throw new AdUnitRegistrationError('request_body_too_large');
  }
  if (occupied.size + prepared.length > MAX_ACTIVE_SLOT_RECORDS) {
    throw new AdUnitRegistrationError('registry_capacity');
  }
  return Object.freeze(prepared);
}

export function addAdUnitsResult(units: readonly ProgrammaticAdUnit[]): AddAdUnitsResult {
  return Object.freeze({ registered: Object.freeze(units.map(({ code }) => code)) });
}

// The mutable merge registry remains connected only to the pre-cutover core entry.
const legacyRegistry = new Map<string, AdUnit>();

export function addAdUnits(units: AdUnit | AdUnit[]): void {
  for (const unit of toArray(units)) {
    if (!unit?.code) continue;
    legacyRegistry.set(unit.code, { ...legacyRegistry.get(unit.code), ...unit });
  }
  log.info('addAdUnits:', { count: toArray(units).length });
}

export function firstSize(unit: AdUnit): Size | null {
  const sizes = unit.mediaTypes?.banner?.sizes;
  return sizes && sizes.length ? sizes[0]! : null;
}

export function getAllUnits(): AdUnit[] {
  return Array.from(legacyRegistry.values());
}

export function getUnit(code: string): AdUnit | undefined {
  return legacyRegistry.get(code);
}
