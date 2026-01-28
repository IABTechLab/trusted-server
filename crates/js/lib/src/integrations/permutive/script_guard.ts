import { createScriptGuard } from '../../shared/script_guard';

/**
 * Check if a URL is a Permutive SDK URL.
 * Matches the logic from permutive.rs:97-101
 */
function isPermutiveSdkUrl(url: string): boolean {
  if (!url) return false;

  const lower = url.toLowerCase();
  return (
    (lower.includes('.edge.permutive.app') || lower.includes('cdn.permutive.com')) &&
    lower.endsWith('-web.js')
  );
}

const guard = createScriptGuard({
  name: 'Permutive',
  isTargetUrl: isPermutiveSdkUrl,
  proxyPath: '/integrations/permutive/sdk',
});

export const installPermutiveGuard = guard.install;
export const isGuardInstalled = guard.isInstalled;
export const resetGuardState = guard.reset;
