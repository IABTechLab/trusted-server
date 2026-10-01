// Immutable browser-facing first-party endpoint resolver for the creative IIFE.
// Capture the injected classic script synchronously during module evaluation so
// later DOM mutations cannot redirect dynamic sign or rebuild requests.
let capturedOrigin: string | null = null;

try {
  const source = document.currentScript?.getAttribute('src') || document.currentScript?.src;
  if (source) capturedOrigin = new URL(source, location.href).origin;
} catch {
  capturedOrigin = null;
}

export function firstPartyOrigin(): string | null {
  if (capturedOrigin) return capturedOrigin;
  try {
    return new URL(location.href).origin;
  } catch {
    return null;
  }
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
