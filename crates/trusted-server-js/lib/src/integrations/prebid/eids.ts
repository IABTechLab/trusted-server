// Plain-data copies of the EIDs Prebid placed in the `trustedServer` bidder
// request, and the `ts-eids` cookie value built from them.
//
// Pure functions: nothing here reads `pbjs`, the DOM or the clock. The server
// validates every entry of the body and the cookie alike (spec 4.4), so this
// module checks nothing beyond what serialization needs.

/** UTF-8 byte budget for the EIDs forwarded in one `/auction` body. */
export const MAX_AUCTION_EIDS_BYTES = 65_536;

/** Maximum length of the base64 `ts-eids` cookie value. */
export const MAX_EID_COOKIE_BYTES = 3072;

/** Counts reported by {@link collectBidderEids} for debug logging. */
export interface EidCollectionStats {
  /** Entries copied into the result. */
  forwarded: number;
  /** Entries dropped because they could not be serialized. */
  unserializable: number;
  /** Entries dropped because the byte budget was exhausted. */
  overBudget: number;
}

/** A fitted `ts-eids` cookie value and the sources trimming touched. */
export interface FittedEidCookie {
  /** Base64 cookie value, or `undefined` when nothing fits. */
  value: string | undefined;
  /** Sources removed whole. */
  droppedSources: string[];
  /** Sources that lost UIDs but stayed in the cookie. */
  trimmedSources: string[];
}

const utf8Encoder = new TextEncoder();

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

/**
 * Returns the arrays Prebid may hold EIDs in, in reading order:
 * `ortb2.user.ext.eids`, then `ortb2.user.eids`. RTD modules write after
 * Prebid normalizes first-party data, so `user.eids` can hold entries too.
 * Prebid redacts both paths for the bidder, so reading both does not widen
 * what `trustedServer` receives.
 */
function bidderEidArrays(bidderRequest: unknown): unknown[][] {
  if (!isRecord(bidderRequest) || !isRecord(bidderRequest.ortb2)) {
    return [];
  }
  const user = bidderRequest.ortb2.user;
  if (!isRecord(user)) {
    return [];
  }
  const arrays: unknown[][] = [];
  if (isRecord(user.ext) && Array.isArray(user.ext.eids)) {
    arrays.push(user.ext.eids);
  }
  if (Array.isArray(user.eids)) {
    arrays.push(user.eids);
  }
  return arrays;
}

function serializeEntry(entry: unknown): string | undefined {
  try {
    // `undefined` and functions serialize to `undefined`; treat them as
    // unserializable like a circular object or a BigInt.
    return JSON.stringify(entry);
  } catch {
    return undefined;
  }
}

/**
 * Copies the EIDs Prebid placed in a `trustedServer` bidder request.
 *
 * Reads `ortb2.user.ext.eids` then `ortb2.user.eids`, copies each entry
 * through a JSON round trip so the result is plain data (`undefined` fields
 * disappear), drops entries that cannot be serialized, and keeps entries in
 * order while their serializations total at most
 * {@link MAX_AUCTION_EIDS_BYTES} UTF-8 bytes, dropping the rest. Entries are
 * not merged and not validated.
 *
 * @param bidderRequest the second `buildRequests` argument
 * @param stats filled with counts for debug logging, when given
 * @returns the copied entries, or `undefined` when none remain
 */
export function collectBidderEids(
  bidderRequest: unknown,
  stats?: EidCollectionStats
): unknown[] | undefined {
  const collected: unknown[] = [];
  let usedBytes = 0;
  let budgetExhausted = false;
  let unserializable = 0;
  let overBudget = 0;

  for (const entries of bidderEidArrays(bidderRequest)) {
    for (const entry of entries) {
      const serialized = serializeEntry(entry);
      if (serialized === undefined) {
        unserializable += 1;
        continue;
      }
      const bytes = utf8Encoder.encode(serialized).length;
      if (budgetExhausted || usedBytes + bytes > MAX_AUCTION_EIDS_BYTES) {
        budgetExhausted = true;
        overBudget += 1;
        continue;
      }
      usedBytes += bytes;
      collected.push(JSON.parse(serialized));
    }
  }

  if (stats) {
    stats.forwarded = collected.length;
    stats.unserializable = unserializable;
    stats.overBudget = overBudget;
  }
  return collected.length > 0 ? collected : undefined;
}

function withoutEidExt(entry: unknown): unknown {
  if (!isRecord(entry) || !('ext' in entry)) {
    return entry;
  }
  const copy: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(entry)) {
    if (key !== 'ext') {
      copy[key] = value;
    }
  }
  return copy;
}

/** JSON with object keys sorted at every level, for equality checks. */
function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) {
    return `[${value.map(canonicalJson).join(',')}]`;
  }
  if (isRecord(value)) {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
      .join(',')}}`;
  }
  return JSON.stringify(value) ?? 'null';
}

function cookieLabel(entry: unknown): string {
  return isRecord(entry) && typeof entry.source === 'string' ? entry.source : '(malformed entry)';
}

function encodeUtf8Base64(text: string): string {
  let binary = '';
  for (const byte of utf8Encoder.encode(text)) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}

/**
 * Builds the `ts-eids` cookie value from a request's collected EIDs.
 *
 * Removes each entry's EID-level `ext`, drops entries that become identical
 * (compared with sorted keys), then trims from the tail until the standard
 * padded base64 of the JSON array's UTF-8 bytes is at most
 * {@link MAX_EID_COOKIE_BYTES} long: drop UIDs from the last entry, then the
 * entry itself, and repeat. A last entry that is not an object with a `uids`
 * array is removed whole. The input is not modified.
 *
 * @param eids the entries forwarded in the `/auction` body
 * @returns the cookie value, or `undefined` when nothing fits, and the
 *   sources trimming touched
 */
export function fitEidCookie(eids: readonly unknown[]): FittedEidCookie {
  const seen = new Set<string>();
  const payload: unknown[] = [];
  for (const entry of eids) {
    const stripped = withoutEidExt(entry);
    const key = canonicalJson(stripped);
    if (seen.has(key)) {
      continue;
    }
    seen.add(key);
    payload.push(
      isRecord(stripped) && Array.isArray(stripped.uids)
        ? { ...stripped, uids: [...stripped.uids] }
        : stripped
    );
  }

  const dropped = new Set<string>();
  const trimmed = new Set<string>();
  while (payload.length > 0) {
    const value = encodeUtf8Base64(JSON.stringify(payload));
    if (value.length <= MAX_EID_COOKIE_BYTES) {
      return {
        value,
        droppedSources: [...dropped],
        trimmedSources: [...trimmed],
      };
    }

    const last = payload[payload.length - 1];
    if (isRecord(last) && Array.isArray(last.uids) && last.uids.length > 1) {
      last.uids = last.uids.slice(0, -1);
      trimmed.add(cookieLabel(last));
      continue;
    }

    payload.pop();
    const label = cookieLabel(last);
    dropped.add(label);
    // A source trimmed and then dropped is reported once, as dropped.
    trimmed.delete(label);
  }

  return {
    value: undefined,
    droppedSources: [...dropped],
    trimmedSources: [...trimmed],
  };
}
