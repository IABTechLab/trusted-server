import { installCreativeInitial } from '../leaf/creative_guard';

import { defineInitialSlice, registerInitialSlice } from './definition';

export const CREATIVE_INITIAL_SLICE = defineInitialSlice(
  'creative_initial',
  installCreativeInitial
);

registerInitialSlice(CREATIVE_INITIAL_SLICE, 3);
