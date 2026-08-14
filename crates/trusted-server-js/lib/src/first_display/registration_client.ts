declare const __TSJS_EMBEDDED_RELEASE_ID_V1__: string;

import type { FirstDisplayComponentRegistrationV1 } from './registration';

/** Submit one immutable component record and the synchronous current-script identity. */
export function registerCurrentFirstDisplayComponent(
  id: string,
  order: number,
  prepare: FirstDisplayComponentRegistrationV1['prepare']
): boolean {
  try {
    const browser = (globalThis as unknown as { window: Window }).window;
    const target = Object.getOwnPropertyDescriptor(browser, 'tsjs')?.value;
    const sink = Object.getOwnPropertyDescriptor(target, '_registerFirstDisplay')?.value;
    return (
      Reflect.apply(sink, target, [
        Object.freeze({
          abi: 1,
          id,
          releaseId: __TSJS_EMBEDDED_RELEASE_ID_V1__,
          order,
          prepare,
        }),
        browser.document.currentScript,
      ]) === true
    );
  } catch {
    return false;
  }
}
