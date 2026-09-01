import { installSourcepointInitial } from '../leaf/consent_snapshot';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

import type { InitialSliceInstaller } from './definition';

export const installSourcepointInitialSlice: InitialSliceInstaller = (candidate, own, config) => {
  const bindings = candidate as Readonly<{ observe: unknown; register: unknown }>;
  return installSourcepointInitial(
    Object.freeze({
      config,
      document,
      observe: bindings.observe,
      origin: location.origin,
      registerRoute: bindings.register,
      storage: window.localStorage,
    }),
    own
  );
};

registerCurrentFirstDisplayComponent('sourcepoint_initial', installSourcepointInitialSlice);
