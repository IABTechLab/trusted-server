import {
  INTEGRATION_CONFIG_IDS_V1,
  type BootJsonValueV1,
  type IntegrationConfigEntryV1,
  type IntegrationConfigIdV1,
  type IntegrationConfigsV1,
} from '../types';

const MAX_DEPTH = 16;
const MAX_VALUES = 4_096;
const MAX_STRING_BYTES = 4_096;
const MAX_ENTRY_BYTES = 65_536;
const MAX_CARRIER_BYTES = 524_288;
const SHA256_INITIAL = Object.freeze([
  0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
]);
const SHA256_ROUNDS = Object.freeze([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

const CONFIG_ORDER = new Map(INTEGRATION_CONFIG_IDS_V1.map((id, index) => [id, index] as const));

function utf8Length(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function rotateRight(value: number, count: number): number {
  return (value >>> count) | (value << (32 - count));
}

/** Calculate SHA-256 synchronously so parser-time boot validation remains effect-inert. */
export function sha256HexUtf8V1(value: string): string {
  const source = new TextEncoder().encode(value);
  const paddedLength = Math.ceil((source.length + 9) / 64) * 64;
  const padded = new Uint8Array(paddedLength);
  padded.set(source);
  padded[source.length] = 0x80;
  const bitLength = source.length * 8;
  const view = new DataView(padded.buffer);
  view.setUint32(paddedLength - 8, Math.floor(bitLength / 0x1_0000_0000));
  view.setUint32(paddedLength - 4, bitLength >>> 0);

  const hash = [...SHA256_INITIAL];
  const words = new Uint32Array(64);
  for (let offset = 0; offset < paddedLength; offset += 64) {
    for (let index = 0; index < 16; index += 1) {
      words[index] = view.getUint32(offset + index * 4);
    }
    for (let index = 16; index < 64; index += 1) {
      const left = words[index - 15]!;
      const right = words[index - 2]!;
      const sigma0 = rotateRight(left, 7) ^ rotateRight(left, 18) ^ (left >>> 3);
      const sigma1 = rotateRight(right, 17) ^ rotateRight(right, 19) ^ (right >>> 10);
      words[index] = (words[index - 16]! + sigma0 + words[index - 7]! + sigma1) >>> 0;
    }

    let [a, b, c, d, e, f, g, h] = hash;
    for (let index = 0; index < 64; index += 1) {
      const sum1 = rotateRight(e!, 6) ^ rotateRight(e!, 11) ^ rotateRight(e!, 25);
      const choose = (e! & f!) ^ (~e! & g!);
      const temporary1 = (h! + sum1 + choose + SHA256_ROUNDS[index]! + words[index]!) >>> 0;
      const sum0 = rotateRight(a!, 2) ^ rotateRight(a!, 13) ^ rotateRight(a!, 22);
      const majority = (a! & b!) ^ (a! & c!) ^ (b! & c!);
      const temporary2 = (sum0 + majority) >>> 0;
      h = g;
      g = f;
      f = e;
      e = (d! + temporary1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (temporary1 + temporary2) >>> 0;
    }
    hash[0] = (hash[0]! + a!) >>> 0;
    hash[1] = (hash[1]! + b!) >>> 0;
    hash[2] = (hash[2]! + c!) >>> 0;
    hash[3] = (hash[3]! + d!) >>> 0;
    hash[4] = (hash[4]! + e!) >>> 0;
    hash[5] = (hash[5]! + f!) >>> 0;
    hash[6] = (hash[6]! + g!) >>> 0;
    hash[7] = (hash[7]! + h!) >>> 0;
  }
  return hash.map((word) => word.toString(16).padStart(8, '0')).join('');
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
