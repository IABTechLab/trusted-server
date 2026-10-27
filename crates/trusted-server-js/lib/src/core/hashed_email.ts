// Writes the first-party `ts-hem` cookie: the lowercase hex SHA-256 of the
// visitor's normalized email address. TS reads it per request as the identity
// providers' hashed-email input and never stores it, forwards it to the
// origin, or sends it to Prebid Server.

const HEM_COOKIE_NAME = 'ts-hem';

const ATTRIBUTES = 'Path=/; Secure; SameSite=Lax';

// Thirty days: the page sets it again on each sign-in.
const HEM_COOKIE_MAX_AGE = 30 * 24 * 60 * 60;

const SHA256_HEX_PATTERN = /^[0-9a-f]{64}$/;

const GMAIL_DOMAINS = new Set(['gmail.com', 'googlemail.com']);

/**
 * Normalizes an email address the way UID2 and most ID vendors hash it:
 * trims and lowercases, and for Gmail drops dots and any `+suffix` from the
 * local part.
 *
 * @param email the address the visitor gave
 * @returns the normalized address, or `undefined` when it is not an address
 */
export function normalizeEmail(email: string): string | undefined {
  const trimmed = email.trim().toLowerCase();
  const at = trimmed.indexOf('@');
  if (at <= 0 || at === trimmed.length - 1 || trimmed.includes('@', at + 1) || /\s/.test(trimmed)) {
    return undefined;
  }
  let local = trimmed.slice(0, at);
  const domain = trimmed.slice(at + 1);
  if (GMAIL_DOMAINS.has(domain)) {
    local = local.split('+')[0].replace(/\./g, '');
    if (local.length === 0) {
      return undefined;
    }
  }
  return `${local}@${domain}`;
}

function writeCookie(value: string, maxAge: number): void {
  try {
    document.cookie = `${HEM_COOKIE_NAME}=${value}; ${ATTRIBUTES}; Max-Age=${maxAge}`;
  } catch {
    // Cookie access can throw in sandboxed frames; the hashed email is optional.
  }
}

/**
 * Stores an already-hashed email.
 *
 * @param hex the SHA-256 of the normalized address, as 64 hex digits
 * @returns whether the value was a digest and was stored
 */
export function setHashedEmail(hex: string): boolean {
  const value = hex.trim().toLowerCase();
  if (!SHA256_HEX_PATTERN.test(value)) {
    return false;
  }
  writeCookie(value, HEM_COOKIE_MAX_AGE);
  return true;
}

/**
 * Normalizes and hashes an email address in the browser, then stores the
 * digest. The address itself is never stored or sent.
 *
 * @param email the address the visitor gave
 * @returns whether a digest was stored
 */
export async function setEmail(email: string): Promise<boolean> {
  const normalized = normalizeEmail(email);
  const subtle = globalThis.crypto?.subtle;
  if (normalized === undefined || subtle === undefined) {
    return false;
  }
  const digest = await subtle.digest('SHA-256', new TextEncoder().encode(normalized));
  const hex = Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join(
    ''
  );
  return setHashedEmail(hex);
}

/** Removes the stored hashed email, for example on sign-out. */
export function clearHashedEmail(): void {
  writeCookie('', 0);
}
