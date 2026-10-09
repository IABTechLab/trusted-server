import type { TraceRequestContextV1 } from './types';

/** Tests an own property without relying on newer browser object helpers. */
export function traceOwn(value: object, key: PropertyKey): boolean {
  return Object.prototype.hasOwnProperty.call(value, key);
}

/** Accepts only plain JSON-shaped objects with their own data properties. */
export function traceObject(value: unknown): value is Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) return false;
  return Reflect.ownKeys(value).every((key) => {
    if (typeof key !== 'string') return false;
    const property = Object.getOwnPropertyDescriptor(value, key);
    return property?.enumerable === true && traceOwn(property, 'value');
  });
}

/** Enforces an explicit object allowlist, including its required keys. */
export function traceKeys(
  value: Record<string, unknown>,
  required: readonly string[],
  optional: readonly string[] = []
): boolean {
  return (
    required.every((key) => traceOwn(value, key)) &&
    Object.keys(value).every((key) => required.includes(key) || optional.includes(key))
  );
}

/** Validates Unicode and printable text before measuring its UTF-8 byte bound. */
export function traceText(value: unknown, limit = 128): value is string {
  if (typeof value !== 'string') return false;
  for (const character of value) {
    const code = character.codePointAt(0)!;
    if (
      code <= 0x1f ||
      (code >= 0x7f && code <= 0x9f) ||
      (code >= 0xd800 && code <= 0xdfff) ||
      code === 0x061c ||
      code === 0x200e ||
      code === 0x200f ||
      (code >= 0x202a && code <= 0x202e) ||
      (code >= 0x2066 && code <= 0x2069)
    )
      return false;
  }
  return new TextEncoder().encode(value).length <= limit;
}

/** Requires an actual calendar date and UTC RFC 3339 serialization. */
export function traceTimestamp(value: unknown): value is string {
  if (typeof value !== 'string') return false;
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d{1,9})?Z$/.exec(value);
  if (!match) return false;
  const [, yearText, monthText, dayText, hourText, minuteText, secondText] = match;
  const year = Number(yearText);
  const month = Number(monthText);
  const day = Number(dayText);
  const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  return (
    month >= 1 &&
    month <= 12 &&
    day >= 1 &&
    day <= days[month - 1] &&
    Number(hourText) <= 23 &&
    Number(minuteText) <= 59 &&
    Number(secondText) <= 59
  );
}

/** Validates a finite nonnegative counter within its explicit upper bound. */
export function traceInteger(value: unknown, maximum = Number.MAX_SAFE_INTEGER): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 && value <= maximum;
}

function maskedAddress(value: unknown): boolean {
  if (!traceText(value)) return false;
  const ipv4 = /^((?:0|[1-9]\d{0,2}))\.((?:0|[1-9]\d{0,2}))\.((?:0|[1-9]\d{0,2}))\.0\/24$/.exec(
    value
  );
  if (ipv4) return ipv4.slice(1).every((octet) => Number(octet) <= 255);
  if (!value.endsWith('::/48')) return false;
  const address = value.slice(0, -3);
  const prefix = address.slice(0, -2);
  if (prefix !== '' && !/^[\da-f]{1,4}(?::[\da-f]{1,4}){0,2}$/.test(prefix)) return false;
  try {
    return new URL(`http://[${address}]/`).hostname === `[${address}]`;
  } catch {
    return false;
  }
}

function cookieHealth(value: unknown, validDetail: string): boolean {
  if (!traceObject(value) || value.source !== 'request') return false;
  if (value.state === 'absent') return traceKeys(value, ['source', 'state']);
  if (!traceKeys(value, ['source', 'state', 'detail']) || typeof value.detail !== 'string')
    return false;
  switch (value.state) {
    case 'present_valid':
      return value.detail === validDetail;
    case 'present_invalid':
      return ['malformed', 'oversized', 'unsupported_value'].includes(value.detail);
    case 'duplicate':
      return value.detail === 'multiple_values';
    case 'unavailable':
      return ['header_too_large', 'header_not_utf8', 'runtime_header_ambiguous'].includes(
        value.detail
      );
    default:
      return false;
  }
}

/** Validates the exact request-context schema, including unavailable cookie facts. */
export function validateTraceRequestContext(value: unknown): value is TraceRequestContextV1 {
  try {
    return requestContext(value);
  } catch {
    return false;
  }
}

function requestContext(value: unknown): value is TraceRequestContextV1 {
  if (
    !traceObject(value) ||
    !traceKeys(value, ['schema_version', 'captured_at', 'network', 'cookies']) ||
    value.schema_version !== 1 ||
    !traceTimestamp(value.captured_at) ||
    !traceObject(value.network) ||
    !traceObject(value.cookies)
  )
    return false;
  const network = value.network;
  const bounds = {
    country: 2,
    region: 32,
    http_version: 32,
    tls_protocol: 32,
    tls_cipher: 32,
    edge_hostname: 128,
    edge_region: 128,
    edge_pop: 32,
  } as const;
  if (!traceKeys(network, [], ['masked_client_ip', 'asn', ...Object.keys(bounds)])) return false;
  if (traceOwn(network, 'masked_client_ip') && !maskedAddress(network.masked_client_ip))
    return false;
  if (traceOwn(network, 'asn') && !traceInteger(network.asn, 0xffff_ffff)) return false;
  for (const [key, bound] of Object.entries(bounds)) {
    if (traceOwn(network, key) && !traceText(network[key], bound)) return false;
  }
  if (typeof network.country === 'string' && !/^[\x20-\x7e]{0,2}$/.test(network.country))
    return false;
  const cookies = value.cookies;
  return (
    traceKeys(cookies, ['ts_ec', 'ts_eids', 'ts_tester', 'diagnostics_session']) &&
    cookieHealth(cookies.ts_ec, 'valid_ec_format') &&
    cookieHealth(cookies.ts_eids, 'valid_eids_format') &&
    cookieHealth(cookies.ts_tester, 'valid_tester_value') &&
    cookieHealth(cookies.diagnostics_session, 'valid_diagnostics_value')
  );
}
