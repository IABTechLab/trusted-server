import { createScriptGuard } from '../../shared/script_guard';
import { parseScriptUrl } from '../../shared/script_url';

/**
 * Check if a URL is a Permutive SDK URL.
 * Match the parsed host and SDK pathname, as in the native rewriter.
 */
function isPermutiveSdkUrl(url: string): boolean {
  const parsed = parseScriptUrl(url);
  return (
    !!parsed &&
    (parsed.hostname === 'cdn.permutive.com' ||
      (parsed.hostname.endsWith('.edge.permutive.app') &&
        parsed.hostname.length > '.edge.permutive.app'.length)) &&
    parsed.pathname.toLowerCase().endsWith('-web.js')
  );
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
