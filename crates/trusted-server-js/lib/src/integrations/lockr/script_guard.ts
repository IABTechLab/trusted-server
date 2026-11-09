import { createScriptGuard } from '../../shared/script_guard';
import { parseSdkNetworkUrl } from '../../shared/sdk_url';

/**
 * Lockr SDK Script Interception Guard
 *
 * Intercepts any dynamically inserted script tag that loads the Lockr SDK
 * and rewrites it to use the first-party domain proxy endpoint. This works
 * across all frameworks (Next.js, Nuxt, Gatsby, vanilla JS, etc.) and catches
 * scripts inserted via appendChild, insertBefore, or any other dynamic DOM
 * manipulation.
 *
 * Built on the shared script_guard factory, which registers with the shared
 * DOM insertion dispatcher to catch dynamic insertions and rewrite SDK URLs to
 * the first-party proxy endpoint without relying on server-side HTML rewriting
 * in client-side scenarios.
 */

/**
 * Check if a URL is a Lockr SDK URL.
 * Browser/Rust ownership cases live in test/fixtures/sdk-url-ownership.json.
 */
function isLockrSdkUrl(url: string): boolean {
  const parsed = parseSdkNetworkUrl(url);
  if (!parsed) return false;

  const path = parsed.pathname.toLowerCase();
  return (
    (parsed.hostname === 'aim.loc.kr' || parsed.hostname === 'identity.loc.kr') &&
    path.includes('identity-lockr') &&
    path.endsWith('.js')
  );
}

const guard = createScriptGuard({
  displayName: 'Lockr',
  id: 'lockr',
  isTargetUrl: isLockrSdkUrl,
  proxyPath: '/integrations/lockr/sdk',
});

/**
 * Install the Lockr guard to intercept dynamic script loading.
 * Registers a handler with the shared DOM insertion dispatcher so dynamically
 * inserted Lockr SDK script elements are rewritten before insertion.
 * Works across all frameworks and vanilla JavaScript.
 */
export const installLockrGuard = guard.install;

/**
 * Check if the guard is currently installed.
 */
export const isGuardInstalled = guard.isInstalled;

/**
 * Reset the guard installation state (primarily for testing).
 */
export const resetGuardState = guard.reset;
