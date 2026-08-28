import { installPermutiveInitial } from '../leaf/context_snapshot';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

import type { InitialSliceInstaller } from './definition';

export const installPermutiveInitialSlice: InitialSliceInstaller = (candidate, own) => {
  const bindings = candidate as Readonly<{ observe: unknown; register: unknown }>;
  return installPermutiveInitial(
    Object.freeze({
      clearTimer: (handle: unknown) => window.clearTimeout(handle as number),
      getSdk: () => Reflect.get(window, 'permutive'),
      host: location.host,
      observe: bindings.observe,
      origin: location.origin,
      protocol: location.protocol,
      readStorage: (key: string) => window.localStorage.getItem(key),
      registerRoute: bindings.register,
      setTimer: (callback: () => void, delayMs: number) => window.setTimeout(callback, delayMs),
    }),
    own
  );
};

registerCurrentFirstDisplayComponent('permutive_initial', installPermutiveInitialSlice);
