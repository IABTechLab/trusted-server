declare const __TSJS_EMBEDDED_RELEASE_ID_V1__: string;

import type { FirstDisplayComponentRegistrationV1 } from '../shared/first_display_registration';

/** Submit one immutable component record and the synchronous current-script identity. */
export function registerCurrentFirstDisplayComponent(
  id: string,
  install: FirstDisplayComponentRegistrationV1['install']
): boolean {
  try {
    const target = window.tsjs as unknown as {
      _registerFirstDisplay?: (candidate: readonly unknown[]) => boolean;
    };
    return (
      target._registerFirstDisplay?.([1, id, __TSJS_EMBEDDED_RELEASE_ID_V1__, install]) === true
    );
  } catch {
    return false;
  }
}
