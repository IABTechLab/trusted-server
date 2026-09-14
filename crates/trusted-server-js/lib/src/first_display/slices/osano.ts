import { installOsanoInitial } from '../leaf/consent_snapshot';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

import type { InitialSliceInstaller } from './definition';

export const installOsanoInitialSlice: InitialSliceInstaller = (candidate, own) =>
  installOsanoInitial(
    Object.freeze({
      clearTimer: (handle: unknown) => window.clearTimeout(handle as number),
      document,
      observe: (candidate as Readonly<{ observe: unknown }>).observe,
      setTimer: (callback: () => void, delayMs: number) => window.setTimeout(callback, delayMs),
      target: window,
    }),
    own
  );

registerCurrentFirstDisplayComponent('osano_initial', installOsanoInitialSlice);
