import { createScriptGuard } from '../../shared/script_guard';
import { parseScriptUrl, scriptUrlPath } from '../../shared/script_url';

const SOURCEPOINT_CDN_HOST = 'cdn.privacy-mgmt.com';

/** Check parsed CDN host ownership. */
export function isSourcepointUrl(url: string): boolean {
  return parseScriptUrl(url)?.hostname === SOURCEPOINT_CDN_HOST;
}

/** Preserve the CDN path, query and fragment on the first-party proxy. */
export function rewriteSourcepointUrl(originalUrl: string): string {
  const parsed = parseScriptUrl(originalUrl);
  if (parsed?.hostname !== SOURCEPOINT_CDN_HOST) return originalUrl;
  return `${window.location.origin}/integrations/sourcepoint/cdn${scriptUrlPath(parsed)}`;
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
