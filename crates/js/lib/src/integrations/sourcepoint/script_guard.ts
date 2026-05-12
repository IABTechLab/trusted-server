import { createScriptGuard } from '../../shared/script_guard';

const SOURCEPOINT_CDN_HOST = 'cdn.privacy-mgmt.com';

function normalizeSourcepointUrl(url: string): string | null {
  if (!url) return null;

  const trimmed = url.trim();
  if (!trimmed) return null;

  if (trimmed.startsWith('//')) return `https:${trimmed}`;
  if (trimmed.startsWith('http://') || trimmed.startsWith('https://')) return trimmed;

  // Keep in sync with Rust `parse_sourcepoint_url()` in:
  // crates/trusted-server-core/src/integrations/sourcepoint.rs
  // (protocol-relative + bare-host handling + hostname-only host checks).
  // Bare domain or path — attempt to parse as https URL.
  // The hostname === check in isSourcepointUrl rejects non-matching domains.
  return `https://${trimmed}`;
}

function parseSourcepointUrl(url: string): URL | null {
  const normalized = normalizeSourcepointUrl(url);
  if (!normalized) return null;

  try {
    return new URL(normalized);
  } catch {
    return null;
  }
}

export function isSourcepointUrl(url: string): boolean {
  const parsed = parseSourcepointUrl(url);
  return parsed?.hostname === SOURCEPOINT_CDN_HOST;
}

export function rewriteSourcepointUrl(originalUrl: string): string {
  const parsed = parseSourcepointUrl(originalUrl);
  if (!parsed) return originalUrl;

  const query = parsed.search || '';

  return `${window.location.origin}/integrations/sourcepoint/cdn${parsed.pathname}${query}`;
}

const guard = createScriptGuard({
  displayName: 'Sourcepoint',
  id: 'sourcepoint',
  isTargetUrl: isSourcepointUrl,
  rewriteUrl: rewriteSourcepointUrl,
});

export const installSourcepointGuard = guard.install;
export const isGuardInstalled = guard.isInstalled;
export const resetGuardState = guard.reset;
