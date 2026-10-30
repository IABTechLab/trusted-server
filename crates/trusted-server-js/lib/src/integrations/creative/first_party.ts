import { normalizeTrustedOrigin, TRUSTED_BASE_URL } from '../../shared/origin';

// Capture the classic script origin synchronously so later currentScript/DOM
// changes cannot change the captured endpoint origin. Its public origin can differ
// from both the document and the rendering parent's stamp, so it takes precedence.
// This source is not authenticated: creative script that runs first can shadow
// currentScript and choose the captured endpoint origin despite the immutable
// parent stamp.
// Only when the script origin is unusable do we fall back to the pinned
// stamp/document/baseURI resolution used by legacy relative clicks. An opaque
// or otherwise unusable base supplies no origin.
const endpointOrigin: string | null = (() => {
  try {
    const source = document.currentScript?.getAttribute('src') || document.currentScript?.src;
    const origin = source && normalizeTrustedOrigin(new URL(source, location.href).origin);
    if (origin) return origin;
  } catch {
    // Fall back to the pinned document origin below.
  }
  return normalizeTrustedOrigin(TRUSTED_BASE_URL) || null;
})();

export function firstPartyOrigin(): string | null {
  return endpointOrigin;
}

export function resolveFirstPartyPath(path: string): string {
  const origin = firstPartyOrigin();
  if (!origin || !path.startsWith('/')) return path;
  try {
    return new URL(path, origin).toString();
  } catch {
    return path;
  }
}

export function isFirstPartyProxyUrl(value: string): boolean {
  const origin = firstPartyOrigin();
  if (!origin) return false;
  try {
    const url = new URL(value, origin);
    return url.origin === origin && url.pathname === '/first-party/proxy';
  } catch {
    return false;
  }
}
