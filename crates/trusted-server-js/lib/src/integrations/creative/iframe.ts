// Dynamic iframe proxy guard: routes iframe src assignments through the first-party proxy.
import { createDynamicSrcProxy } from './dynamic_src_guard';
import { shouldProxyExternalUrl, signProxyUrl } from './proxy_sign';

const installProxy = createDynamicSrcProxy<HTMLIFrameElement>({
  elementConstructor: typeof HTMLIFrameElement === 'undefined' ? undefined : HTMLIFrameElement,
  selector: 'iframe[src]',
  tagName: 'iframe',
  resourceName: 'iframe',
  logPrefix: 'tsjs-creative:iframe',
  shouldProxy: (raw) => shouldProxyExternalUrl(raw),
  signProxy: (raw) => signProxyUrl(raw),
});

export function installDynamicIframeProxy(): void {
  installProxy();
}
