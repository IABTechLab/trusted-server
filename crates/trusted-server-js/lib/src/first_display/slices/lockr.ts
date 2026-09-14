import { installLockrInitial } from '../leaf/route_guard';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

import type { InitialSliceInstaller } from './definition';

export const installLockrInitialSlice: InitialSliceInstaller = (candidate, own) => {
  const bindings = candidate as Readonly<{ observe: unknown; register: unknown }>;
  return installLockrInitial(
    Object.freeze({
      clearTimer: (handle: unknown) => window.clearTimeout(handle as number),
      getSdk: () => Reflect.get(window, 'identityLockr'),
      host: location.host,
      observe: bindings.observe,
      origin: location.origin,
      protocol: location.protocol,
      register: bindings.register,
      setTimer: (callback: () => void, delayMs: number) => window.setTimeout(callback, delayMs),
    }),
    own
  );
};

registerCurrentFirstDisplayComponent('lockr_initial', installLockrInitialSlice);
