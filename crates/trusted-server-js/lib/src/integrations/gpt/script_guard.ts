import { createScriptGuard } from '../../shared/script_guard';

const GPT_DOMAIN = 'securepubads.g.doubleclick.net';
const PROXY_PREFIX = '/integrations/gpt';

function parseUrl(url: string): URL | undefined {
  if (!url) return undefined;
  try {
    return new URL(url, typeof window === 'undefined' ? undefined : window.location.href);
  } catch {
    if (!url.startsWith('//')) return undefined;
    try {
      return new URL(`https:${url}`);
    } catch {
      return undefined;
    }
  }
}

function isGptDomainUrl(url: string): boolean {
  return parseUrl(url)?.hostname.toLowerCase() === GPT_DOMAIN;
}

function rewriteGptUrl(originalUrl: string): string {
  const parsed = parseUrl(originalUrl);
  if (!parsed || typeof window === 'undefined') return originalUrl;
  return `${window.location.origin}${PROXY_PREFIX}${parsed.pathname}${parsed.search}${parsed.hash}`;
}

const guard = createScriptGuard({
  deepInterception: { documentWriteUrlHint: GPT_DOMAIN },
  displayName: 'GPT',
  id: 'gpt',
  isTargetUrl: isGptDomainUrl,
  rewriteUrl: rewriteGptUrl,
});

export function installGptGuard(): void {
  guard.install();
}

export function isGuardInstalled(): boolean {
  return guard.isInstalled();
}

export function resetGuardState(): void {
  guard.reset();
}
