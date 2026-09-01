// Dynamic iframe proxy guard: routes iframe src assignments through the first-party proxy.
import { createDynamicSrcProxy } from './dynamic_src_guard';
import { shouldProxyExternalUrl, signProxyUrl } from './proxy_sign';
import type { CreativeGuardHandle } from './startup';

const installProxy = createDynamicSrcProxy<HTMLIFrameElement>({
  elementConstructor: typeof HTMLIFrameElement === 'undefined' ? undefined : HTMLIFrameElement,
  selector: 'iframe[src]',
  tagName: 'iframe',
  resourceName: 'iframe',
  logPrefix: 'tsjs-creative:iframe',
  shouldProxy: (raw) => shouldProxyExternalUrl(raw),
  signProxy: (raw) => signProxyUrl(raw),
});

export function installDynamicIframeProxy(scanInitially = true): CreativeGuardHandle {
  return installProxy(scanInitially);
}
