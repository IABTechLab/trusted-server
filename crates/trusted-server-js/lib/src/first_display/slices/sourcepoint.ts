import { installSourcepointInitial } from '../leaf/consent_snapshot';
import { registerFirstDisplayBrowserRoute } from '../leaf/browser_route_owner';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

import type { InitialSliceInstaller } from './definition';

export const installSourcepointInitialSlice: InitialSliceInstaller = (candidate, own, config) =>
  installSourcepointInitial(
    Object.freeze({
      config,
      document,
      observe: (candidate as Readonly<{ observe: unknown }>).observe,
      origin: location.origin,
      registerRoute: registerFirstDisplayBrowserRoute,
      storage: window.localStorage,
    }),
    own
  );

registerCurrentFirstDisplayComponent('sourcepoint_initial', installSourcepointInitialSlice);
