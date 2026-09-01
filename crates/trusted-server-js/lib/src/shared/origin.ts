// Origin helpers shared by creative runtime modules.

// Normalize a candidate first-party origin, returning '' when it is unusable.
//
// Parsing with `URL` rather than matching origin grammar by hand keeps valid
// but less common forms working — notably IPv6 literals such as
// `http://[::1]:7676`, which a DNS-shaped pattern rejects. `URL.origin`
// serializes back to `scheme://host[:port]`, so the result is also what gets
// embedded in the srcdoc stamp.
//
// The final character check is defence in depth for that embedding: an http(s)
// origin cannot contain quotes, backslashes, angle brackets, or whitespace, so
// anything that does is not the value we think it is and is discarded rather
// than written into the document.
export function normalizeTrustedOrigin(candidate: unknown): string {
  if (typeof candidate !== 'string' || !candidate) return '';
  try {
    const parsed = new URL(candidate);
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return '';
    const origin = parsed.origin;
    if (!origin || origin === 'null' || /['"<>\\\s]/.test(origin)) return '';
    return origin;
  } catch {
    return '';
  }
}

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
    const stamped = normalizeTrustedOrigin(
      (window as { __tsCreativeOrigin?: unknown }).__tsCreativeOrigin
    );
    if (stamped) return stamped;
  } catch {
    // fall through
  }
  try {
    const origin = normalizeTrustedOrigin(location.origin);
    if (origin) return origin;
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

// Exact first-party origin for protocol messages and root-owned endpoints.
// `TRUSTED_BASE_URL` may be a full inherited base URI in the final fallback,
// so normalize it to its origin while retaining the same HTTP(S)-only and
// credential-free trust boundary.
export function trustedHttpOrigin(baseUrl: string = TRUSTED_BASE_URL): string {
  if (!baseUrl) return '';
  try {
    const parsed = new URL(baseUrl);
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return '';
    if (parsed.username !== '' || parsed.password !== '') return '';
    return parsed.origin;
  } catch {
    return '';
  }
}

export function trustedDocumentHttpOrigin(
  documentOrigin: string,
  trustedBaseUrl: string = TRUSTED_BASE_URL
): string {
  return trustedHttpOrigin(documentOrigin === 'null' ? trustedBaseUrl : documentOrigin);
}
