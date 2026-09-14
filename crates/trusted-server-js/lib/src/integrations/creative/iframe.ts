/** @file Routes dynamic creative iframe sources through the first-party proxy. */
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

/** Install the idempotent dynamic-iframe source guard. */
export function installDynamicIframeProxy(): void {
  installProxy();
}
