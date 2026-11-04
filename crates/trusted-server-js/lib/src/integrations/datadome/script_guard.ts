import { createScriptGuard } from '../../shared/script_guard';
import { parseScriptUrl, scriptUrlPath } from '../../shared/script_url';

/**
 * DataDome SDK Script Interception Guard
 *
 * Intercepts any dynamically inserted script tag that loads the DataDome SDK
 * and rewrites it to use the first-party domain proxy endpoint. This works
 * across all frameworks (Next.js, Nuxt, Gatsby, vanilla JS, etc.) and catches
 * scripts inserted via appendChild, insertBefore, or any other dynamic DOM
 * manipulation.
 *
 * Built on the shared script_guard factory, which registers with the shared
 * DOM insertion dispatcher and preserves the original DataDome path
 * (e.g., /tags.js, /js/check).
 */

/** Check parsed SDK host ownership, without promoting relative paths to hosts. */
function isDataDomeSdkUrl(url: string): boolean {
  return parseScriptUrl(url)?.hostname === 'js.datadome.co';
}

/** Extract the parsed path, query and fragment of an HTTP source reference. */
function extractDataDomePath(url: string): string {
  const parsed = parseScriptUrl(url);
  return parsed ? scriptUrlPath(parsed) : '';
}

/** Keep the SDK path on the first-party proxy. */
function rewriteDataDomeUrl(originalUrl: string): string {
  if (!isDataDomeSdkUrl(originalUrl)) return originalUrl;
  return `${window.location.origin}/integrations/datadome${extractDataDomePath(originalUrl)}`;
}

const guard = createScriptGuard({
  displayName: 'DataDome',
  id: 'datadome',
  isTargetUrl: isDataDomeSdkUrl,
  rewriteUrl: rewriteDataDomeUrl,
});

/**
 * Install the DataDome guard to intercept dynamic script loading.
 * Registers a handler with the shared DOM insertion dispatcher so dynamically
 * inserted DataDome SDK script elements are rewritten before insertion.
 * Works across all frameworks and vanilla JavaScript.
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
