import { installLockrInitial } from '../leaf/route_guard';
import { registerFirstDisplayBrowserRoute } from '../leaf/browser_route_owner';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

import type { InitialSliceInstaller } from './definition';

export const installLockrInitialSlice: InitialSliceInstaller = (candidate, own) =>
  installLockrInitial(
    Object.freeze({
      clearTimer: (handle: unknown) => window.clearTimeout(handle as number),
      getSdk: () => Reflect.get(window, 'identityLockr'),
      host: location.host,
      observe: (candidate as Readonly<{ observe: unknown }>).observe,
      origin: location.origin,
      protocol: location.protocol,
      register: registerFirstDisplayBrowserRoute,
      setTimer: (callback: () => void, delayMs: number) => window.setTimeout(callback, delayMs),
    }),
    own
  );

registerCurrentFirstDisplayComponent('lockr_initial', installLockrInitialSlice);
