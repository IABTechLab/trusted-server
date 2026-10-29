import { createScriptGuard } from '../../shared/script_guard';
import { parseSdkNetworkUrl } from '../../shared/sdk_url';

/**
 * Check if a URL is a Permutive SDK URL.
 * Browser/Rust ownership cases live in test/fixtures/sdk-url-ownership.json.
 */
function isPermutiveSdkUrl(url: string): boolean {
  const parsed = parseSdkNetworkUrl(url);
  if (!parsed) return false;

  const host = parsed.hostname;
  const edgeSuffix = '.edge.permutive.app';
  return (
    (host === 'cdn.permutive.com' ||
      (host.endsWith(edgeSuffix) && host.length > edgeSuffix.length)) &&
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
