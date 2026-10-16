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
  // Use the same ASCII whitespace and explicit-authority rules as Rust.
  const value = url.replace(/^[ \t\n\r\f\v]+|[ \t\n\r\f\v]+$/g, '');
  if (!/^(?:https?:)?\/\//i.test(value)) return false;

  try {
    const parsed = new URL(value.startsWith('//') ? `https:${value}` : value);
    const path = parsed.pathname.toLowerCase();
    return (
      (parsed.hostname === 'aim.loc.kr' || parsed.hostname === 'identity.loc.kr') &&
      path.includes('identity-lockr') &&
      path.endsWith('.js')
    );
  } catch {
    return false;
  }
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
