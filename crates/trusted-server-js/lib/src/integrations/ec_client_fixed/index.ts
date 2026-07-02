// Demonstration client for the client-cycle Edge Cookie provider (client-fixed).
//
// Client and server share one fixed, known word. When no Edge Cookie is present,
// this posts that word to the resolve endpoint. With the `client-fixed` provider
// selected, the server verifies the word and, on a match, sets it as an HttpOnly
// Edge Cookie on the response, so the page script never reads it back.
//
// The value is verifiable precisely because it is a known constant, which is the
// point of the demo. It is useless in production, because a fixed value is not an
// identity and every client posts the same word. For demonstration and testing
// only. A real client-cycle provider posts and verifies a real payload (for
// example an OWID signature) instead of a shared constant.
import { log } from '../../core/log';

const EC_COOKIE_NAME = 'ts-ec';
const RESOLVE_ENDPOINT = '/_ts/api/v1/ec/resolve';

// The fixed, known word shared with the server. Must match EXPECTED_VALUE in
// crates/trusted-server-core/src/ec/provider.rs; the two copies are kept in
// sync by hand.
const FIXED_WORD = 'an-ec';

// Returns true when the `ts-ec` cookie is already present in `cookieString`.
export function hasEdgeCookie(cookieString: string): boolean {
  return cookieString.split(';').some((part) => part.trim().startsWith(`${EC_COOKIE_NAME}=`));
}

// Posts the fixed known word to the resolve endpoint when no Edge Cookie is
// present. Returns the word posted, or null when nothing was sent (a cookie
// already exists, or the environment lacks `document`/`fetch`).
export async function resolveEdgeCookie(): Promise<string | null> {
  if (typeof document === 'undefined' || typeof fetch !== 'function') {
    return null;
  }
  if (hasEdgeCookie(document.cookie)) {
    return null;
  }

  try {
    await fetch(RESOLVE_ENDPOINT, {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'text/plain' },
      body: FIXED_WORD,
    });
    log.info('ec client-fixed: posted the known word to the resolve endpoint');
    return FIXED_WORD;
  } catch (err) {
    log.warn('ec client-fixed: resolve request failed', err);
    return null;
  }
}

if (typeof window !== 'undefined') {
  void resolveEdgeCookie();
}
