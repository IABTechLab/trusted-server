import { installDidomiInitial } from '../leaf/config_guard';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

import type { InitialSliceInstaller } from './definition';

export const installDidomiInitialSlice: InitialSliceInstaller = (candidate, own, config) =>
  installDidomiInitial(
    Object.freeze({
      config,
      observe: (candidate as Readonly<{ observe: unknown }>).observe,
      target: window,
    }),
    own
  );

registerCurrentFirstDisplayComponent('didomi_initial', installDidomiInitialSlice);
