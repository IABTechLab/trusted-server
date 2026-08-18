import { installTestlightInitial } from '../leaf/callback_capture';

import { defineInitialSlice, registerInitialSlice } from './definition';

export const TESTLIGHT_INITIAL_SLICE = defineInitialSlice(
  'testlight_initial',
  installTestlightInitial
);

registerInitialSlice(TESTLIGHT_INITIAL_SLICE, 13);
