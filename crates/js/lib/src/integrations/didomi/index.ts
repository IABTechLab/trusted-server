import { log } from '../../core/log';

const DEFAULT_CONSENT_PROXY_PATH = '/integrations/didomi/consent/';

type DidomiConfig = {
  sdkPath?: string;
  [key: string]: unknown;
};

type DidomiWindow = Window & {
  didomiConfig?: DidomiConfig;
  __tsjs_didomi?: { proxyPath?: string };
};

/** Read the server-injected proxy path, falling back to the default. */
function getConsentProxyPath(win: DidomiWindow): string {
  return win.__tsjs_didomi?.proxyPath ?? DEFAULT_CONSENT_PROXY_PATH;
}

function buildProxySdkPath(win: DidomiWindow): string {
  const proxyPath = getConsentProxyPath(win);
  const base = win.location?.origin ?? win.location?.href;
  if (!base) return proxyPath;
  const url = new URL(proxyPath, base);
  return `${url.origin}${url.pathname}`;
}

export function installDidomiSdkProxy(): boolean {
  if (typeof window === 'undefined') return false;

  const win = window as DidomiWindow;
  const config = (win.didomiConfig ??= {});
  const previousSdkPath =
    typeof config.sdkPath === 'string' && config.sdkPath.length > 0
      ? config.sdkPath
      : 'https://sdk.privacy-center.org/';

  const proxiedSdkPath = buildProxySdkPath(win);
  config.sdkPath = proxiedSdkPath;

  log.info('didomi sdkPath overridden for trusted server proxy', {
    previousSdkPath,
    sdkPath: proxiedSdkPath,
  });

  return true;
}

if (typeof window !== 'undefined') {
  installDidomiSdkProxy();
}

export default installDidomiSdkProxy;
