import { parseScriptUrl, scriptUrlPath } from '../../shared/script_url';
import { createBeaconGuard } from '../../shared/beacon_guard';
import { createScriptGuard } from '../../shared/script_guard';

/**
 * Google Tag Manager Script Interception Guard
 *
 * Intercepts dynamically inserted script tags that load GTM or Google Analytics
 * and rewrites their URLs to use the first-party proxy endpoint. This catches
 * scripts inserted via appendChild, insertBefore, or any other dynamic DOM
 * manipulation (e.g. Next.js dynamic imports).
 *
 * Built on the shared script_guard factory with custom URL rewriting to preserve
 * the original path and query string.
 */

/** Hosts and paths served by the native GTM/GA proxy. */
const GTM_HOSTS = ['www.googletagmanager.com', 'www.google-analytics.com', 'analytics.google.com'];
const SUPPORTED_PATHS = ['/gtm.js', '/gtag/js', '/gtag.js', '/collect', '/g/collect'];

/** Check the parsed host and exact routed pathname. */
function isGtmUrl(url: string): boolean {
  const parsed = parseScriptUrl(url);
  return (
    !!parsed && GTM_HOSTS.includes(parsed.hostname) && SUPPORTED_PATHS.includes(parsed.pathname)
  );
}

/** Extract the parsed path, query and fragment of an HTTP source reference. */
function extractGtmPath(url: string): string {
  const parsed = parseScriptUrl(url);
  return parsed ? scriptUrlPath(parsed) : '';
}

/** Preserve the owned GTM/GA source on the first-party proxy. */
function rewriteGtmUrl(originalUrl: string): string {
  if (!isGtmUrl(originalUrl)) return originalUrl;
  return `${window.location.origin}/integrations/google_tag_manager${extractGtmPath(originalUrl)}`;
}

const guard = createScriptGuard({
  displayName: 'GTM',
  id: 'google_tag_manager',
  isTargetUrl: isGtmUrl,
  rewriteUrl: rewriteGtmUrl,
});

const beaconGuard = createBeaconGuard({
  name: 'GTM',
  isTargetUrl: isGtmUrl,
  rewriteUrl: rewriteGtmUrl,
});

export const installGtmGuard = guard.install;
export const isGuardInstalled = guard.isInstalled;
export const resetGuardState = guard.reset;

export const installGtmBeaconGuard = beaconGuard.install;
export const isBeaconGuardInstalled = beaconGuard.isInstalled;
export const resetBeaconGuardState = beaconGuard.reset;

// Export for testing
export { isGtmUrl, extractGtmPath, rewriteGtmUrl };
