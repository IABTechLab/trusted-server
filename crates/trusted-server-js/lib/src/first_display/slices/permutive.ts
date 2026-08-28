import { installPermutiveInitial } from '../leaf/context_snapshot';
import { registerFirstDisplayBrowserRoute } from '../leaf/browser_route_owner';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

import type { InitialSliceInstaller } from './definition';

export const installPermutiveInitialSlice: InitialSliceInstaller = (candidate, own) =>
  installPermutiveInitial(
    Object.freeze({
      clearTimer: (handle: unknown) => window.clearTimeout(handle as number),
      getSdk: () => Reflect.get(window, 'permutive'),
      host: location.host,
      observe: (candidate as Readonly<{ observe: unknown }>).observe,
      origin: location.origin,
      protocol: location.protocol,
      readStorage: (key: string) => window.localStorage.getItem(key),
      registerContext: () => () => undefined,
      registerRoute: registerFirstDisplayBrowserRoute,
      setTimer: (callback: () => void, delayMs: number) => window.setTimeout(callback, delayMs),
    }),
    own
  );

registerCurrentFirstDisplayComponent('permutive_initial', installPermutiveInitialSlice);
