// Dynamic image proxy guard: intercepts <img> sources and routes them via first-party proxy.
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

// Prepare global hooks so every img.src assignment flows through Trusted Server first.
export function installDynamicImageProxy(): void {
  installProxy();
}
