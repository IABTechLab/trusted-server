// Writes the client-only `__Host-ts-eids-s` cookie from the mirror payload
// `/auction` and `/_ts/page-bids` return (spec 6.2).
//
// The cookie is read by page scripts, never by TS: the only read here is the
// comparison that skips an unchanged write. Nothing is copied into `ts-eids`
// or the `/auction` body, so a mirror ID never becomes a browser candidate.

const MIRROR_COOKIE_NAME = '__Host-ts-eids-s';

// base64url without padding, the only alphabet the server emits.
const MIRROR_VALUE_PATTERN = /^[A-Za-z0-9_-]+$/;

const ATTRIBUTES = 'Path=/; Secure; SameSite=Lax';

function currentMirrorValue(): string | undefined {
  const prefix = `${MIRROR_COOKIE_NAME}=`;
  for (const part of document.cookie.split(';')) {
    const candidate = part.trim();
    if (candidate.startsWith(prefix)) {
      return candidate.slice(prefix.length);
    }
  }
  return undefined;
}

/**
 * Applies a mirror payload to `__Host-ts-eids-s`.
 *
 * `{ value, max_age }` writes the cookie unless it already holds `value`.
 * `{ clear: true }` expires it. Anything else, an absent payload included,
 * leaves the cookie as it is.
 *
 * @param payload the `ts_eids_s` member of an `/auction` or page-bids response
 */
export function writeEidsMirror(payload: unknown): void {
  if (payload === null || typeof payload !== 'object') {
    return;
  }
  const { value, max_age: maxAge, clear } = payload as Record<string, unknown>;
  try {
    if (clear === true) {
      document.cookie = `${MIRROR_COOKIE_NAME}=; ${ATTRIBUTES}; Max-Age=0`;
      return;
    }
    if (
      typeof value !== 'string' ||
      !MIRROR_VALUE_PATTERN.test(value) ||
      typeof maxAge !== 'number' ||
      !Number.isInteger(maxAge) ||
      maxAge < 0
    ) {
      return;
    }
    if (currentMirrorValue() === value) {
      return;
    }
    document.cookie = `${MIRROR_COOKIE_NAME}=${value}; ${ATTRIBUTES}; Max-Age=${maxAge}`;
  } catch {
    // Cookie access can throw in sandboxed frames; the mirror is optional.
  }
}
