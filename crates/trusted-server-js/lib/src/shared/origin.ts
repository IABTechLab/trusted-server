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

// Base URL for resolving the root-relative first-party URLs the server-side
// rewriter emits. Pinned at module-load time, in descending order of trust:
//
//  1. `window.__tsCreativeOrigin` — stamped into the srcdoc document by the
//     first-party parent page before any creative markup (see
//     `core/render.ts`). This is the only source that is neither inherited nor
//     `<base>`-sensitive, so it wins wherever present.
//  2. `location.origin` — correct and `<base>`-immune whenever the document has
//     a real origin (creatives rendered outside the sandboxed srcdoc path).
//  3. `document.baseURI` — last resort inside an unstamped `srcdoc`, where
//     `location.href` is `about:srcdoc` and `new URL` rejects it as a base.
//     Inherited from the embedder and therefore honours a publisher `<base>`.
//
// Pinning at load time means bidder script cannot redirect resolution later by
// injecting `<base>`; the server-side rewriter also strips `<base>` from
// creative markup whenever rewriting is enabled.
export const TRUSTED_BASE_URL: string = (() => {
  try {
    const stamped = (window as { __tsCreativeOrigin?: unknown }).__tsCreativeOrigin;
    if (typeof stamped === 'string' && /^https?:\/\/[a-z0-9.-]+(:\d+)?$/i.test(stamped)) {
      return stamped;
    }
  } catch {
    // fall through
  }
  try {
    const origin = location.origin;
    if (origin && origin !== 'null') return origin;
  } catch {
    // fall through
  }
  try {
    const base = typeof document !== 'undefined' ? document.baseURI : '';
    if (base && base !== 'about:srcdoc') return base;
  } catch {
    // fall through
  }
  return '';
})();
