import { createFirstDisplayGoogletagBatch } from '../adapters/googletag';
import { installGptInitial } from '../leaf/gpt_protocol';

import { defineInitialSlice, registerInitialSlice } from './definition';

export const GPT_INITIAL_SLICE = defineInitialSlice('gpt_initial', (candidate, own) =>
  installGptInitial(candidate, own, (input, protocol) =>
    createFirstDisplayGoogletagBatch({ ...input, protocol })
  )
);

registerInitialSlice(GPT_INITIAL_SLICE, 7);
