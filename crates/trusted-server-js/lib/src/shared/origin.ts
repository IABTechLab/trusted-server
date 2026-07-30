// Origin helpers shared by creative runtime modules.

// A sandboxed srcdoc creative without `allow-same-origin` runs in an opaque
// origin: every fetch from it is cross-origin (`Origin: null`), and any
// preflighted request fails against endpoints that do not answer CORS.
// Callers use this to skip doomed requests and take a no-fetch path instead
// (navigation fallback, unsigned resource). Returns true when the origin
// cannot be determined — failing toward the no-fetch path is always safe.
export function hasOpaqueOrigin(): boolean {
  try {
    return typeof window !== 'undefined' && window.origin === 'null';
  } catch {
    return true;
  }
}

// Base URL for resolving root-relative first-party URLs. Inside the sandboxed
// `srcdoc` creative iframe `location.href` is `about:srcdoc`, which `new URL`
// rejects as a base; `document.baseURI` inherits the parent document's URL
// instead. The value is pinned at module-load time so a bidder script that
// later injects a `<base>` element cannot redirect resolution (the server-side
// rewriter also strips `<base>` from creative markup before injecting this
// runtime).
export const TRUSTED_BASE_URL: string = (() => {
  try {
    const base = typeof document !== 'undefined' ? document.baseURI : '';
    if (base && base !== 'about:srcdoc') return base;
  } catch {
    // fall through to location
  }
  try {
    return location.href;
  } catch {
    return '';
  }
})();
