import {
  INTEGRATION_CONFIG_IDS_V1,
  type BootJsonValueV1,
  type IntegrationConfigEntryV1,
  type IntegrationConfigIdV1,
  type IntegrationConfigsV1,
} from '../types';

import { sha256HexUtf8V1 } from './sha256';

export { sha256HexUtf8V1 } from './sha256';

const MAX_DEPTH = 16;
const MAX_VALUES = 4_096;
const MAX_STRING_BYTES = 4_096;
const MAX_ENTRY_BYTES = 65_536;
const MAX_CARRIER_BYTES = 524_288;

const CONFIG_ORDER = new Map(INTEGRATION_CONFIG_IDS_V1.map((id, index) => [id, index] as const));

function utf8Length(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function ownPlainDataRecord(value: unknown): Record<string, unknown> | undefined {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
  if (Object.getPrototypeOf(value) !== Object.prototype) return undefined;
  const result: Record<string, unknown> = {};
  for (const key of Reflect.ownKeys(value)) {
    if (typeof key !== 'string') return undefined;
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
    Object.defineProperty(result, key, {
      configurable: true,
      enumerable: true,
      value: descriptor.value,
      writable: true,
    });
  }
  return result;
}

function exactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}

function snapshotArray(value: unknown, maximum: number): readonly unknown[] | undefined {
  if (!Array.isArray(value) || Object.getPrototypeOf(value) !== Array.prototype) return undefined;
  const length = Object.getOwnPropertyDescriptor(value, 'length');
  if (!length || !('value' in length) || !Number.isSafeInteger(length.value)) return undefined;
  if (length.value < 0 || length.value > maximum) return undefined;
  const keys = Reflect.ownKeys(value);
  if (keys.length !== length.value + 1) return undefined;
  const result: unknown[] = [];
  for (let index = 0; index < length.value; index += 1) {
    const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
    if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
    result.push(descriptor.value);
  }
  return result;
}

function snapshotJsonValue(
  candidate: unknown,
  depth: number,
  seen: Set<object>,
  count: { value: number }
): BootJsonValueV1 | undefined {
  if (candidate === null || typeof candidate === 'boolean') {
    count.value += 1;
    return count.value <= MAX_VALUES ? candidate : undefined;
  }
  if (typeof candidate === 'number') {
    count.value += 1;
    return count.value <= MAX_VALUES && Number.isFinite(candidate) ? candidate : undefined;
  }
  if (typeof candidate === 'string') {
    count.value += 1;
    return count.value <= MAX_VALUES && utf8Length(candidate) <= MAX_STRING_BYTES
      ? candidate
      : undefined;
  }
  if (typeof candidate !== 'object' || depth > MAX_DEPTH || seen.has(candidate)) {
    return undefined;
  }
  seen.add(candidate);
  count.value += 1;
  if (count.value > MAX_VALUES) return undefined;

  const array = snapshotArray(candidate, MAX_VALUES);
  if (array) {
    const copy: BootJsonValueV1[] = [];
    for (const value of array) {
      const accepted = snapshotJsonValue(value, depth + 1, seen, count);
      if (accepted === undefined && value !== null) return undefined;
      copy.push(accepted as BootJsonValueV1);
    }
    return Object.freeze(copy);
  }

  const record = ownPlainDataRecord(candidate);
  if (!record) return undefined;
  const copy: Record<string, BootJsonValueV1> = {};
  for (const [key, value] of Object.entries(record)) {
    if (utf8Length(key) > MAX_STRING_BYTES) return undefined;
    const accepted = snapshotJsonValue(value, depth + 1, seen, count);
    if (accepted === undefined && value !== null) return undefined;
    Object.defineProperty(copy, key, {
      configurable: true,
      enumerable: true,
      value: accepted as BootJsonValueV1,
      writable: true,
    });
  }
  return Object.freeze(copy);
}

/** Copy and recursively freeze the sole generic browser configuration carrier. */
export function snapshotIntegrationConfigsV1(candidate: unknown): IntegrationConfigsV1 | undefined {
  try {
    const root = ownPlainDataRecord(candidate);
    if (!root || !exactKeys(root, ['version', 'entries']) || root.version !== 1) return undefined;
    const entries = snapshotArray(root.entries, INTEGRATION_CONFIG_IDS_V1.length);
    if (!entries) return undefined;
    const seen = new Set<object>();
    const count = { value: 0 };
    const accepted = [];
    let previous = -1;
    for (const candidateEntry of entries) {
      const entry = ownPlainDataRecord(candidateEntry);
      if (!entry || !exactKeys(entry, ['id', 'config']) || typeof entry.id !== 'string') {
        return undefined;
      }
      const order = CONFIG_ORDER.get(entry.id as IntegrationConfigIdV1);
      if (order === undefined || order <= previous) return undefined;
      const config = snapshotJsonValue(entry.config, 0, seen, count);
      if (
        !config ||
        Array.isArray(config) ||
        typeof config !== 'object' ||
        utf8Length(JSON.stringify({ id: entry.id, config })) > MAX_ENTRY_BYTES
      ) {
        return undefined;
      }
      accepted.push(
        Object.freeze({
          id: entry.id as IntegrationConfigIdV1,
          config: config as Readonly<Record<string, BootJsonValueV1>>,
        })
      );
      previous = order;
    }
    const result: IntegrationConfigsV1 = Object.freeze({
      version: 1,
      entries: Object.freeze(accepted),
    });
    return utf8Length(JSON.stringify(result)) <= MAX_CARRIER_BYTES ? result : undefined;
  } catch {
    return undefined;
  }
}

/** Return one already-frozen product value without exposing the carrier map. */
export function integrationConfigValueV1(
  carrier: IntegrationConfigsV1,
  id: IntegrationConfigIdV1
): IntegrationConfigEntryV1['config'] | undefined {
  return carrier.entries.find((entry) => entry.id === id)?.config;
}

/** Bind a retained carrier snapshot to first-display takeover metadata. */
export function canonicalIntegrationConfigDigestV1(carrier: IntegrationConfigsV1): string {
  return sha256HexUtf8V1(JSON.stringify(carrier));
}
