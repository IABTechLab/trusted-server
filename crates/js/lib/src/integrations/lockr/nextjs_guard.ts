import { createScriptGuard } from '../../shared/script_guard';

/**
 * Lockr SDK Script Interception Guard
 *
 * Intercepts any dynamically inserted script tag that loads the Lockr SDK
 * and rewrites it to use the first-party domain proxy endpoint. This works
 * across all frameworks (Next.js, Nuxt, Gatsby, vanilla JS, etc.) and catches
 * scripts inserted via appendChild, insertBefore, or any other dynamic DOM
 * manipulation.
 *
 * Built on the shared script_guard factory which patches DOM methods to catch
 * dynamic insertions and rewrite SDK URLs to use the first-party domain proxy
 * endpoint, bypassing the need for server-side HTML rewriting in dynamic
 * client-side scenarios.
 */

/**
 * Check if a URL is a Lockr SDK URL.
 * Matches the logic from lockr.rs:79-86
 */
function isLockrSdkUrl(url: string): boolean {
  if (!url) return false;

  const lower = url.toLowerCase();

  // Check for aim.loc.kr domain
  if (lower.includes('aim.loc.kr')) {
    return true;
  }

  // Check for identity.loc.kr with identity-lockr and .js extension
  if (
    lower.includes('identity.loc.kr') &&
    lower.includes('identity-lockr') &&
    lower.endsWith('.js')
  ) {
    return true;
  }

  return false;
}

const guard = createScriptGuard({
  name: 'Lockr',
  isTargetUrl: isLockrSdkUrl,
  proxyPath: '/integrations/lockr/sdk',
});

/**
 * Install the Lockr guard to intercept dynamic script loading.
 * Patches Element.prototype.appendChild and insertBefore to catch
 * ANY dynamically inserted Lockr SDK script elements and rewrite their URLs
 * before insertion. Works across all frameworks and vanilla JavaScript.
 */
export const installNextJsGuard = guard.install;

/**
 * Check if the guard is currently installed.
 */
export const isGuardInstalled = guard.isInstalled;

/**
 * Reset the guard installation state (primarily for testing).
 */
export const resetGuardState = guard.reset;
