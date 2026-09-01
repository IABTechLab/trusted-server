import { installCreativeInitial } from '../leaf/creative_guard';
import { registerCurrentFirstDisplayComponent } from '../registration_client';
import { installClickGuard } from '../../integrations/creative/click';
import { installDynamicIframeProxy } from '../../integrations/creative/iframe';
import { installDynamicImageProxy } from '../../integrations/creative/image';

import type { InitialSliceInstaller } from './definition';

const installCreativeInitialSlice: InitialSliceInstaller = (candidate, own, config) =>
  installCreativeInitial(
    Object.freeze({
      document,
      installClickGuard: () => installClickGuard(false),
      installDynamicIframeProxy: () => installDynamicIframeProxy(false),
      installDynamicImageProxy: () => installDynamicImageProxy(false),
      observe: (candidate as Readonly<{ observe: unknown }>).observe,
    }),
    own,
    config
  );

registerCurrentFirstDisplayComponent('creative_initial', installCreativeInitialSlice);
