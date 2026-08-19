const BASE64URL_ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';
const IDENTITY_FAILURE = Object.freeze({
  ok: false as const,
  reason: 'identity_generation_failed' as const,
});

/** Browser-compatible source for cryptographically secure random bytes. */
export type RandomValuesSource = (target: Uint8Array<ArrayBuffer>) => Uint8Array<ArrayBuffer>;

/** The sole failure reported when secure identity generation is unavailable. */
export type IdentityGenerationFailure = typeof IDENTITY_FAILURE;

/** Result of creating or minting one browser identity. */
export type IdentityGenerationResult<T> =
  Readonly<{ ok: true; value: T }> | IdentityGenerationFailure;

/** Observational identity failure callback that never receives identity material. */
export type IdentityFailureObserver = (reason: 'identity_generation_failed') => void;

/** Navigation-local issuer for non-repeating attempt identities. */
export interface NavigationIdentityIssuer {
  readonly adoptFirstDisplayState: (prefix: string, nextOrdinal: number) => boolean;
  readonly adoptNextAttemptOrdinal: (nextOrdinal: number) => boolean;
  readonly mintAttemptId: () => IdentityGenerationResult<string>;
  readonly snapshotPrefix: () => string;
  /** Return a frozen ordinal copy for deterministic tests. */
  readonly snapshotOrdinalForTest: () => readonly [number, number];
}

/** Test-only controls for a deterministic navigation identity issuer. */
export interface TestNavigationIdentityIssuerOptions {
  readonly getRandomValues?: RandomValuesSource;
  readonly initialOrdinal?: readonly [number, number];
  readonly onFailure?: IdentityFailureObserver;
}

function reportFailure(observer?: IdentityFailureObserver): IdentityGenerationFailure {
  try {
    observer?.('identity_generation_failed');
  } catch {
    // Identity failure observation cannot change the typed failure.
  }
  return IDENTITY_FAILURE;
}

function randomBytes(
  length: number,
  source: RandomValuesSource | undefined,
  observer?: IdentityFailureObserver
): IdentityGenerationResult<Uint8Array> {
  if (!source) return reportFailure(observer);
  try {
    const bytes = new Uint8Array(new ArrayBuffer(length));
    const returned = source(bytes);
    if (returned !== bytes || bytes.length !== length) return reportFailure(observer);
    return Object.freeze({ ok: true as const, value: bytes });
  } catch {
    return reportFailure(observer);
  }
}

function encodeBase64Url(bytes: Uint8Array): string {
  let encoded = '';
  for (let index = 0; index < bytes.length; index += 3) {
    const first = bytes[index] ?? 0;
    const second = bytes[index + 1] ?? 0;
    const third = bytes[index + 2] ?? 0;
    const remaining = bytes.length - index;
    const combined = (first << 16) | (second << 8) | third;
    encoded += BASE64URL_ALPHABET[(combined >>> 18) & 63];
    encoded += BASE64URL_ALPHABET[(combined >>> 12) & 63];
    if (remaining > 1) encoded += BASE64URL_ALPHABET[(combined >>> 6) & 63];
    if (remaining > 2) encoded += BASE64URL_ALPHABET[combined & 63];
  }
  return encoded;
}

function decodeBase64UrlPrefix(value: string): Uint8Array<ArrayBuffer> | undefined {
  if (!/^[A-Za-z0-9_-]{11}$/.test(value)) return undefined;
  const bytes = new Uint8Array(new ArrayBuffer(8));
  let byteIndex = 0;
  let bits = 0;
  let buffer = 0;
  for (const character of value) {
    const code = BASE64URL_ALPHABET.indexOf(character);
    if (code < 0) return undefined;
    buffer = (buffer << 6) | code;
    bits += 6;
    while (bits >= 8) {
      bits -= 8;
      if (byteIndex >= bytes.length) return undefined;
      bytes[byteIndex] = (buffer >>> bits) & 0xff;
      byteIndex += 1;
      buffer &= bits === 0 ? 0 : (1 << bits) - 1;
    }
  }
  if (byteIndex !== bytes.length || bits !== 2 || buffer !== 0) return undefined;
  return bytes;
}

function validUnsignedWord(value: number): boolean {
  return Number.isInteger(value) && value >= 0 && value <= 0xffff_ffff;
}

function createNavigationIdentityIssuer(
  source: RandomValuesSource | undefined,
  initialOrdinal: readonly [number, number],
  observer?: IdentityFailureObserver
): IdentityGenerationResult<NavigationIdentityIssuer> {
  if (!validUnsignedWord(initialOrdinal[0]) || !validUnsignedWord(initialOrdinal[1])) {
    return reportFailure(observer);
  }
  const prefixResult = randomBytes(8, source, observer);
  if (!prefixResult.ok) return prefixResult;
  let prefix: Uint8Array<ArrayBuffer>;
  try {
    prefix = new Uint8Array(new ArrayBuffer(8));
    prefix.set(prefixResult.value);
  } catch {
    return reportFailure(observer);
  }
  let highWord = initialOrdinal[0] >>> 0;
  let lowWord = initialOrdinal[1] >>> 0;
  let adoptionOpen = highWord === 0 && lowWord === 0;

  const issuer: NavigationIdentityIssuer = Object.freeze({
    adoptFirstDisplayState(prefixValue: string, nextOrdinal: number): boolean {
      const adoptedPrefix = decodeBase64UrlPrefix(prefixValue);
      if (
        !adoptionOpen ||
        highWord !== 0 ||
        lowWord !== 0 ||
        !adoptedPrefix ||
        !Number.isInteger(nextOrdinal) ||
        nextOrdinal < 1 ||
        nextOrdinal > 0xffff_ffff
      ) {
        return false;
      }
      prefix = adoptedPrefix;
      lowWord = (nextOrdinal - 1) >>> 0;
      adoptionOpen = false;
      return true;
    },
    adoptNextAttemptOrdinal(nextOrdinal: number): boolean {
      if (
        !adoptionOpen ||
        highWord !== 0 ||
        lowWord !== 0 ||
        !Number.isInteger(nextOrdinal) ||
        nextOrdinal < 1 ||
        nextOrdinal > 0xffff_ffff
      ) {
        return false;
      }
      lowWord = (nextOrdinal - 1) >>> 0;
      adoptionOpen = false;
      return true;
    },
    mintAttemptId(): IdentityGenerationResult<string> {
      adoptionOpen = false;
      if (highWord === 0xffff_ffff && lowWord === 0xffff_ffff) {
        return reportFailure(observer);
      }
      try {
        const nextHighWord = lowWord === 0xffff_ffff ? (highWord + 1) >>> 0 : highWord;
        const nextLowWord = lowWord === 0xffff_ffff ? 0 : (lowWord + 1) >>> 0;
        const identity = new Uint8Array(16);
        identity.set(prefix, 0);
        const view = new DataView(identity.buffer);
        view.setUint32(8, nextHighWord, false);
        view.setUint32(12, nextLowWord, false);
        if (identity.byteLength !== 16) return reportFailure(observer);
        for (let index = 0; index < prefix.length; index += 1) {
          if (identity[index] !== prefix[index]) return reportFailure(observer);
        }
        if (
          identity[8] !== nextHighWord >>> 24 ||
          identity[9] !== ((nextHighWord >>> 16) & 0xff) ||
          identity[10] !== ((nextHighWord >>> 8) & 0xff) ||
          identity[11] !== (nextHighWord & 0xff) ||
          identity[12] !== nextLowWord >>> 24 ||
          identity[13] !== ((nextLowWord >>> 16) & 0xff) ||
          identity[14] !== ((nextLowWord >>> 8) & 0xff) ||
          identity[15] !== (nextLowWord & 0xff)
        ) {
          return reportFailure(observer);
        }
        const encoded = encodeBase64Url(identity);
        if (encoded.length !== 22) return reportFailure(observer);
        const result = Object.freeze({ ok: true as const, value: `a1_${encoded}` });
        highWord = nextHighWord;
        lowWord = nextLowWord;
        return result;
      } catch {
        return reportFailure(observer);
      }
    },
    snapshotOrdinalForTest: () => Object.freeze([highWord, lowWord] as const),
    snapshotPrefix: () => encodeBase64Url(prefix),
  });
  return Object.freeze({ ok: true, value: issuer });
}

/** Create one navigation issuer from an explicitly scoped secure-random source. */
export function createNavigationIdentityIssuerFromSource(
  source: RandomValuesSource,
  observer?: IdentityFailureObserver
): IdentityGenerationResult<NavigationIdentityIssuer> {
  return createNavigationIdentityIssuer(source, [0, 0], observer);
}

function browserRandomSource(): RandomValuesSource | undefined {
  try {
    if (typeof crypto !== 'object' || typeof crypto.getRandomValues !== 'function') {
      return undefined;
    }
    return (target: Uint8Array<ArrayBuffer>) => {
      crypto.getRandomValues(target);
      return target;
    };
  } catch {
    return undefined;
  }
}

/** Create a production navigation issuer from browser Web Crypto only. */
export function createBrowserNavigationIdentityIssuer(): IdentityGenerationResult<NavigationIdentityIssuer> {
  const source = browserRandomSource();
  return source ? createNavigationIdentityIssuerFromSource(source) : IDENTITY_FAILURE;
}

/** Create a deterministic navigation issuer for tests only. */
export function createTestNavigationIdentityIssuer(
  options: TestNavigationIdentityIssuerOptions
): IdentityGenerationResult<NavigationIdentityIssuer> {
  return createNavigationIdentityIssuer(
    options.getRandomValues,
    options.initialOrdinal ?? [0, 0],
    options.onFailure
  );
}

function mintFreshIdentity(
  prefix: 't1_' | 'b1_' | 'n1_',
  source: RandomValuesSource | undefined,
  observer?: IdentityFailureObserver
): IdentityGenerationResult<string> {
  const bytes = randomBytes(16, source, observer);
  if (!bytes.ok) return bytes;
  return Object.freeze({ ok: true, value: `${prefix}${encodeBase64Url(bytes.value)}` });
}

/** Mint one production lifecycle ticket from sixteen browser Web Crypto bytes. */
export function mintBrowserLifecycleTicket(): IdentityGenerationResult<string> {
  return mintFreshIdentity('t1_', browserRandomSource());
}

/** Mint one production bootstrap nonce from sixteen browser Web Crypto bytes. */
export function mintBrowserBootstrapNonce(): IdentityGenerationResult<string> {
  return mintFreshIdentity('b1_', browserRandomSource());
}

/** Mint one production renderer nonce from sixteen browser Web Crypto bytes. */
export function mintBrowserRendererNonce(): IdentityGenerationResult<string> {
  return mintFreshIdentity('n1_', browserRandomSource());
}

/** Mint one deterministic lifecycle ticket for tests only. */
export function mintTestLifecycleTicket(
  source: RandomValuesSource,
  observer?: IdentityFailureObserver
): IdentityGenerationResult<string> {
  return mintFreshIdentity('t1_', source, observer);
}

/** Mint one deterministic bootstrap nonce for tests only. */
export function mintTestBootstrapNonce(
  source: RandomValuesSource,
  observer?: IdentityFailureObserver
): IdentityGenerationResult<string> {
  return mintFreshIdentity('b1_', source, observer);
}

/** Mint one deterministic renderer nonce for tests only. */
export function mintTestRendererNonce(
  source: RandomValuesSource,
  observer?: IdentityFailureObserver
): IdentityGenerationResult<string> {
  return mintFreshIdentity('n1_', source, observer);
}
