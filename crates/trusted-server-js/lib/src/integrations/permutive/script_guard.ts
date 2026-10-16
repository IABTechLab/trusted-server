import { createScriptGuard } from '../../shared/script_guard';

/**
 * Check if a URL is a Permutive SDK URL.
 * Browser/Rust ownership cases live in test/fixtures/sdk-url-ownership.json.
 */
function isPermutiveSdkUrl(url: string): boolean {
  // Use the same ASCII whitespace and explicit-authority rules as Rust.
  const value = url.replace(/^[ \t\n\r\f\v]+|[ \t\n\r\f\v]+$/g, '');
  if (!/^(?:https?:)?\/\//i.test(value)) return false;

  try {
    const parsed = new URL(value.startsWith('//') ? `https:${value}` : value);
    const host = parsed.hostname;
    const edgeSuffix = '.edge.permutive.app';
    return (
      (host === 'cdn.permutive.com' ||
        (host.endsWith(edgeSuffix) && host.length > edgeSuffix.length)) &&
      parsed.pathname.toLowerCase().endsWith('-web.js')
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
