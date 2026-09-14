import { createFirstDisplayGoogletagBatch } from '../adapters/googletag';
import { installGptInitial } from '../leaf/gpt_protocol';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

import type { InitialSliceInstaller } from './definition';

export const installGptInitialSlice: InitialSliceInstaller = (candidate, own, config) =>
  installGptInitial(
    candidate,
    own,
    (input, protocol) =>
      createFirstDisplayGoogletagBatch({
        browser: input[0],
        clearTimer: input[1],
        document: input[2],
        setTimer: input[3],
        projection: input[4],
        ...(input[5] === undefined ? {} : { diagnosticsActive: input[5] }),
        ...(input[6] === undefined ? {} : { onNativeMutation: input[6] }),
        protocol,
      }),
    config
  );

registerCurrentFirstDisplayComponent('gpt_initial', installGptInitialSlice);
