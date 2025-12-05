import { log } from '../../core/log';

const DEFAULT_SDK_PATH = 'https://sdk.privacy-center.org/';
const CONSENT_PROXY_PATH = '/didomi/consent/';

type DidomiConfig = {
  sdkPath?: string;
  [key: string]: unknown;
};

type DidomiWindow = Window & { didomiConfig?: DidomiConfig };

function buildProxySdkPath(win: DidomiWindow): string {
  const base = win.location?.origin ?? win.location?.href;
  if (!base) return CONSENT_PROXY_PATH;
  const url = new URL(CONSENT_PROXY_PATH, base);
  return `${url.origin}${url.pathname}`;
}

export function installDidomiSdkProxy(): boolean {
  if (typeof window === 'undefined') return false;

  const win = window as DidomiWindow;
  const config = (win.didomiConfig ??= {});
  const previousSdkPath =
    typeof config.sdkPath === 'string' && config.sdkPath.length > 0
      ? config.sdkPath
      : DEFAULT_SDK_PATH;

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
