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

/** Regex to match GTM/GA domains: www.googletagmanager.com, www.google-analytics.com, analytics.google.com */
const GTM_URL_PATTERN =
  /^(?:https?:)?(?:\/\/)?(www\.(googletagmanager|google-analytics)\.com|analytics\.google\.com)(?:\/|$)/i;

/**
 * Supported paths that the server can proxy.
 * Must match the route patterns defined in the GoogleTagManagerIntegration handler
 * in crates/common/src/integrations/google_tag_manager.rs
 */
const SUPPORTED_PATHS = ['/gtm.js', '/gtag/js', '/gtag.js', '/collect', '/g/collect'];

/**
 * Check if a URL is a GTM or Google Analytics URL with a supported path.
 * Matches the logic from google_tag_manager.rs GTM_URL_PATTERN.
 *
 * Valid patterns:
 * - https://www.googletagmanager.com/gtm.js?id=GTM-XXXX
 * - https://www.google-analytics.com/g/collect
 * - https://analytics.google.com/g/collect
 * - //www.googletagmanager.com/gtm.js?id=GTM-XXXX
 *
 * Invalid:
 * - https://googletagmanager.com/gtm.js (missing www.)
 * - https://example.com/www.googletagmanager.com (domain mismatch)
 * - https://www.googletagmanager.com/ns.html (unsupported path)
 */
function isGtmUrl(url: string): boolean {
  if (!url || !GTM_URL_PATTERN.test(url)) {
    return false;
  }

  // Extract path from URL to validate it's a supported route
  try {
    const normalizedUrl = url.startsWith('//')
      ? `https:${url}`
      : url.startsWith('http')
        ? url
        : `https://${url}`;

    const parsed = new URL(normalizedUrl);
    const path = parsed.pathname;

    // Check if the path matches any of our supported paths
    // Note: pathname never includes query strings, so exact match is sufficient
    return SUPPORTED_PATHS.some((supportedPath) => path === supportedPath);
  } catch {
    // Fail closed: if URL parsing fails, reject the URL rather than
    // using a permissive fallback that could match malformed strings
    return false;
  }
}

/**
 * Extract the path and query string from a GTM/GA URL.
 * e.g., "https://www.googletagmanager.com/gtm.js?id=GTM-XXXX" -> "/gtm.js?id=GTM-XXXX"
 *       "https://www.google-analytics.com/g/collect?v=2" -> "/g/collect?v=2"
 */
function extractGtmPath(url: string): string {
  try {
    const normalizedUrl = url.startsWith('//')
      ? `https:${url}`
      : url.startsWith('http')
        ? url
        : `https://${url}`;

    const parsed = new URL(normalizedUrl);
    return parsed.pathname + parsed.search;
  } catch (error) {
    // Fallback: extract path after the domain using regex
    console.warn('[GTM Guard] URL parsing failed for:', url, 'Error:', error);
    const match = url.match(
      /(?:www\.(?:googletagmanager|google-analytics)\.com|analytics\.google\.com)(\/[^'"\s]*)/i
    );
    if (!match || !match[1]) {
      console.warn('[GTM Guard] Fallback regex failed, using default path /gtm.js');
      return '/gtm.js';
    }
    return match[1];
  }
}

/**
 * Rewrite a GTM/GA URL to the first-party proxy path.
 */
function rewriteGtmUrl(originalUrl: string): string {
  return `${window.location.origin}/integrations/google_tag_manager${extractGtmPath(originalUrl)}`;
}

const guard = createScriptGuard({
  name: 'GTM',
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
