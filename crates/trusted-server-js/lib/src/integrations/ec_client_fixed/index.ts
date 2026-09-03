// Demonstration client for the client-cycle Edge Cookie provider (client-fixed).
//
// Client and server share one fixed, known word. When the resolved marker is
// absent, this posts that word to the resolve endpoint. With the `client-fixed`
// provider selected, the server verifies the word and, on a match, persists the
// identity-graph row and sets the coded form of the word (cfix~an-ec) as an
// HttpOnly Edge Cookie on the
// response, together with a non-HttpOnly marker cookie. The Edge Cookie itself
// is HttpOnly, so this script can never see it; the marker is what tells it a
// resolve already succeeded, so it does not post again on every page view.
//
// The value is verifiable precisely because it is a known constant, which is the
// point of the demo. It is useless in production, because a fixed value is not an
// identity and every client posts the same word. For demonstration and testing
// only. A real client-cycle provider posts and verifies a real payload (for
// example an OWID signature) instead of a shared constant.
import { log } from '../../core/log';

const RESOLVE_ENDPOINT = '/_ts/api/v1/ec/resolve';

// The non-HttpOnly companion the server sets alongside the Edge Cookie. Must
// match COOKIE_TS_EC_RESOLVED in crates/trusted-server-core/src/constants.rs;
// a Rust test asserts the two stay in sync.
const MARKER_COOKIE_NAME = 'ts-ecr';

// The fixed, known word shared with the server. Must match EXPECTED_VALUE in
// crates/trusted-server-core/src/ec/provider.rs; a Rust test asserts the two
// stay in sync.
const FIXED_WORD = 'an-ec';

// The permission this provider requires, the same declaration the server-side
// provider makes in `required_permissions` (crates/trusted-server-core/src/ec/
// provider.rs). A page module is treated like any other provider: it declares
// what it requires and checks that against the resolved state the server
// hands the page before it does anything. The server enforces the same gate on
// the resolve endpoint, so this check is the page's half of one decision, not
// a substitute for the server's. A Rust test asserts the two stay in sync.
const REQUIRED_PERMISSION = 'necessary.operations.storage';

// Waits for the resolved permission state the edge injects into the page
// (`window.tsjs.permissions`, via `tsjs.whenPermissions()`) and returns
// whether the permission this module requires is set. With no permission state
// on the page there is nothing to check against, so the answer is no.
export async function requiredPermissionIsSet(): Promise<boolean> {
  const whenPermissions = window.tsjs?.whenPermissions;
  if (typeof whenPermissions !== 'function') {
    log.warn('ec client-fixed: no permission state on the page, not posting');
    return false;
  }
  const snapshot = await whenPermissions();
  return Array.isArray(snapshot?.set) && snapshot.set.includes(REQUIRED_PERMISSION);
}

// Returns true when the resolved marker is present in `cookieString`. The Edge
// Cookie itself is HttpOnly and never appears in `document.cookie`, so the
// marker is the only signal the page has.
export function hasResolvedMarker(cookieString: string): boolean {
  return cookieString.split(';').some((part) => part.trim().startsWith(`${MARKER_COOKIE_NAME}=`));
}

// Posts the fixed known word to the resolve endpoint when no resolved marker is
// present and the required permission is set. Returns the word posted, or null
// when nothing was sent or the post failed (a resolve already succeeded, the
// required permission is not set, the environment lacks `document`/`fetch`, or
// the request threw).
export async function resolveEdgeCookie(): Promise<string | null> {
  if (typeof document === 'undefined' || typeof fetch !== 'function') {
    return null;
  }
  if (hasResolvedMarker(document.cookie)) {
    return null;
  }
  if (!(await requiredPermissionIsSet())) {
    log.info('ec client-fixed: required permission not set, not posting');
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
