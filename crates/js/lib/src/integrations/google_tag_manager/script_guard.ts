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

/** Regex to match www.googletagmanager.com or www.google-analytics.com as domains */
const GTM_URL_PATTERN =
  /^(?:https?:)?(?:\/\/)?www\.(googletagmanager|google-analytics)\.com(?:\/|$)/i;

/**
 * Check if a URL is a GTM or Google Analytics URL.
 * Matches the logic from google_tag_manager.rs GTM_URL_PATTERN.
 *
 * Valid patterns:
 * - https://www.googletagmanager.com/gtm.js?id=GTM-XXXX
 * - https://www.google-analytics.com/g/collect
 * - //www.googletagmanager.com/gtm.js?id=GTM-XXXX
 *
 * Invalid:
 * - https://googletagmanager.com/gtm.js (missing www.)
 * - https://example.com/www.googletagmanager.com (domain mismatch)
 */
function isGtmUrl(url: string): boolean {
  return !!url && GTM_URL_PATTERN.test(url);
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
  } catch {
    // Fallback: extract path after the domain
    console.debug('[GTM Guard] URL parsing failed, using fallback for:', url);
    const match = url.match(/www\.(?:googletagmanager|google-analytics)\.com(\/[^'"\s]*)/i);
    return match?.[1] || '/gtm.js';
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

export const installGtmGuard = guard.install;
export const isGuardInstalled = guard.isInstalled;
export const resetGuardState = guard.reset;

// Export for testing
export { isGtmUrl, extractGtmPath, rewriteGtmUrl };
