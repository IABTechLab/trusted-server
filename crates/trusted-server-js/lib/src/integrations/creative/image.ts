/** @file Routes dynamic creative image sources through the first-party proxy. */
import { createDynamicSrcProxy } from './dynamic_src_guard';
import { shouldProxyExternalUrl, signProxyUrl } from './proxy_sign';

// NOTE: This module intentionally logs at info level in the hot paths so that when
// creatives crash before reaching a console, we still have breadcrumbs showing how
// the image proxy reacted (install, observed src, signing, etc.). Keep these logs
// because they are invaluable for field debugging when a creative injects pixels
// at unexpected times.

const installProxy = createDynamicSrcProxy<HTMLImageElement>({
  elementConstructor: typeof HTMLImageElement === 'undefined' ? undefined : HTMLImageElement,
  selector: 'img[src]',
  tagName: 'img',
  factoryName: 'Image',
  resourceName: 'image',
  logPrefix: 'tsjs-creative:image',
  shouldProxy: (raw) => shouldProxyExternalUrl(raw),
  signProxy: (raw) => signProxyUrl(raw),
});

/** Install the idempotent image source guard, including the global `Image` factory. */
export function installDynamicImageProxy(): void {
  installProxy();
}
