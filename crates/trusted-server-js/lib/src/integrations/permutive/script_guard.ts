import { createScriptGuard } from '../../shared/script_guard';

/**
 * Check if a URL is a Permutive SDK URL.
 * Matches the logic from permutive.rs:97-101
 */
function isPermutiveSdkUrl(url: string): boolean {
  if (!url) return false;
  try {
    const parsed = new URL(url);
    const hostname = parsed.hostname.toLowerCase();
    return (
      parsed.protocol === 'https:' &&
      (hostname === 'cdn.permutive.com' || hostname.endsWith('.edge.permutive.app')) &&
      parsed.pathname.toLowerCase().endsWith('-web.js') &&
      parsed.search === '' &&
      parsed.hash === ''
    );
  } catch {
    return false;
  }
}

const guard = createScriptGuard({
  displayName: 'Permutive',
  id: 'permutive',
  isTargetUrl: isPermutiveSdkUrl,
  proxyPath: '/integrations/permutive/sdk',
});

export const installPermutiveGuard = guard.install;
export const isGuardInstalled = guard.isInstalled;
export const resetGuardState = guard.reset;
