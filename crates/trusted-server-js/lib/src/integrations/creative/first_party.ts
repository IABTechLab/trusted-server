import { normalizeTrustedOrigin, TRUSTED_BASE_URL } from '../../shared/origin';

// Capture the classic script origin synchronously so later currentScript/DOM
// changes cannot change the captured origins. The script origin chooses endpoints;
// server-configured metadata only recognizes already-proxied resources on another
// origin. Neither source is authenticated: creative script that runs first can
// shadow currentScript and choose both origins despite the immutable parent stamp.
// Only when the script origin is unusable do we fall back to the pinned
// stamp/document/baseURI resolution used by legacy relative clicks. An opaque
// or otherwise unusable base supplies no origin.
const { endpointOrigin, configuredProxyOrigin } = (() => {
  let scriptOrigin = '';
  let configuredProxyOrigin = '';
  try {
    const script = document.currentScript;
    configuredProxyOrigin = normalizeTrustedOrigin(script?.getAttribute('data-ts-public-origin'));
    const source = script?.getAttribute('src') || script?.src;
    scriptOrigin = (source && normalizeTrustedOrigin(new URL(source, location.href).origin)) || '';
  } catch {
    // Fall back to the pinned document origin below.
  }
  return {
    endpointOrigin: scriptOrigin || normalizeTrustedOrigin(TRUSTED_BASE_URL) || null,
    configuredProxyOrigin,
  };
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
  try {
    const url = new URL(value, endpointOrigin || undefined);
    return (
      url.pathname === '/first-party/proxy' &&
      (url.origin === endpointOrigin || url.origin === configuredProxyOrigin)
    );
  } catch {
    return false;
  }
}
