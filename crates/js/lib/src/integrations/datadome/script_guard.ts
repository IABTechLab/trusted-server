import { createScriptGuard } from '../../shared/script_guard';

/**
 * DataDome SDK Script Interception Guard
 *
 * Intercepts any dynamically inserted script tag that loads the DataDome SDK
 * and rewrites it to use the first-party domain proxy endpoint. This works
 * across all frameworks (Next.js, Nuxt, Gatsby, vanilla JS, etc.) and catches
 * scripts inserted via appendChild, insertBefore, or any other dynamic DOM
 * manipulation.
 *
 * Built on the shared script_guard factory with custom URL rewriting to preserve
 * the original path from the DataDome URL (e.g., /tags.js, /js/check).
 */

/** Regex to match js.datadome.co as a domain in URLs */
const DATADOME_URL_PATTERN = /^(?:https?:)?\/\/js\.datadome\.co(?:\/|$)|^js\.datadome\.co(?:\/|$)/i;

/**
 * Check if a URL is a DataDome SDK URL.
 * Matches URLs where js.datadome.co is the host (not just a substring).
 *
 * Valid patterns:
 * - https://js.datadome.co/...
 * - //js.datadome.co/...
 * - js.datadome.co/... (bare domain)
 *
 * Invalid:
 * - https://cdn.example.com/js.datadome.co.js (domain is not js.datadome.co)
 */
function isDataDomeSdkUrl(url: string): boolean {
  return !!url && DATADOME_URL_PATTERN.test(url);
}

/**
 * Extract the path from a DataDome URL to preserve it in the rewrite.
 * e.g., "https://js.datadome.co/tags.js" -> "/tags.js"
 *       "https://js.datadome.co/js/check" -> "/js/check"
 */
function extractDataDomePath(url: string): string {
  try {
    // Normalize to absolute URL for parsing
    const normalizedUrl = url.startsWith('//')
      ? `https:${url}`
      : url.startsWith('http')
        ? url
        : `https://${url}`;

    const parsed = new URL(normalizedUrl);
    return parsed.pathname + parsed.search;
  } catch {
    // Fallback: extract path after js.datadome.co
    const match = url.match(/js\.datadome\.co(\/[^'"]*)?/i);
    return match?.[1] || '/tags.js';
  }
}

/**
 * Build a first-party URL from the current page origin and the DataDome path.
 */
function rewriteDataDomeUrl(originalUrl: string): string {
  return `${window.location.origin}/integrations/datadome${extractDataDomePath(originalUrl)}`;
}

const guard = createScriptGuard({
  name: 'DataDome',
  isTargetUrl: isDataDomeSdkUrl,
  rewriteUrl: rewriteDataDomeUrl,
});

/**
 * Install the DataDome guard to intercept dynamic script loading.
 * Patches Element.prototype.appendChild and insertBefore to catch
 * ANY dynamically inserted DataDome SDK script elements and rewrite their URLs
 * before insertion. Works across all frameworks and vanilla JavaScript.
 */
export const installDataDomeGuard = guard.install;

/**
 * Check if the guard is currently installed.
 */
export const isGuardInstalled = guard.isInstalled;

/**
 * Reset the guard installation state (primarily for testing).
 */
export const resetGuardState = guard.reset;

// Export for testing
export { isDataDomeSdkUrl, extractDataDomePath, rewriteDataDomeUrl };
