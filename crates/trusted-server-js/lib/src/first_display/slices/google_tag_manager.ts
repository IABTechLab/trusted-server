import { installGoogleTagManagerInitial, type FirstDisplayRouteRuleV1 } from '../leaf/route_guard';
import { registerFirstDisplayBrowserRoute } from '../leaf/browser_route_owner';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

import type { InitialSliceInstaller } from './definition';

export const installGoogleTagManagerInitialSlice: InitialSliceInstaller = (candidate, own) =>
  installGoogleTagManagerInitial(
    Object.freeze({
      observe: (candidate as Readonly<{ observe: unknown }>).observe,
      origin: location.origin,
      register: (rule: FirstDisplayRouteRuleV1) => registerFirstDisplayBrowserRoute(rule, true),
    }),
    own
  );

registerCurrentFirstDisplayComponent(
  'google_tag_manager_initial',
  installGoogleTagManagerInitialSlice
);
