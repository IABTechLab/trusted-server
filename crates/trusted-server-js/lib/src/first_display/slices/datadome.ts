import { installDataDomeInitial } from '../leaf/route_guard';
import { registerFirstDisplayBrowserRoute } from '../leaf/browser_route_owner';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

import type { InitialSliceInstaller } from './definition';

export const installDataDomeInitialSlice: InitialSliceInstaller = (candidate, own) =>
  installDataDomeInitial(
    Object.freeze({
      observe: (candidate as Readonly<{ observe: unknown }>).observe,
      origin: location.origin,
      register: registerFirstDisplayBrowserRoute,
    }),
    own
  );

registerCurrentFirstDisplayComponent('datadome_initial', installDataDomeInitialSlice);
