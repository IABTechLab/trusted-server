import { installGoogleTagManagerInitial } from '../leaf/route_guard';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

import type { InitialSliceInstaller } from './definition';

export const installGoogleTagManagerInitialSlice: InitialSliceInstaller = (candidate, own) => {
  const bindings = candidate as Readonly<{ observe: unknown; register: unknown }>;
  return installGoogleTagManagerInitial(
    Object.freeze({
      observe: bindings.observe,
      origin: location.origin,
      register: bindings.register,
    }),
    own
  );
};

registerCurrentFirstDisplayComponent(
  'google_tag_manager_initial',
  installGoogleTagManagerInitialSlice
);
