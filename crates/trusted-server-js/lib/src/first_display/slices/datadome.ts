import { installDataDomeInitial } from '../leaf/route_guard';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

import type { InitialSliceInstaller } from './definition';

export const installDataDomeInitialSlice: InitialSliceInstaller = (candidate, own) => {
  const bindings = candidate as Readonly<{ observe: unknown; register: unknown }>;
  return installDataDomeInitial(
    Object.freeze({
      observe: bindings.observe,
      origin: location.origin,
      register: bindings.register,
    }),
    own
  );
};

registerCurrentFirstDisplayComponent('datadome_initial', installDataDomeInitialSlice);
