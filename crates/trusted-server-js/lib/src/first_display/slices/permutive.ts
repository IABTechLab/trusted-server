import { installPermutiveInitial } from '../leaf/context_snapshot';

import { defineInitialSlice, registerInitialSlice } from './definition';

export const PERMUTIVE_INITIAL_SLICE = defineInitialSlice(
  'permutive_initial',
  installPermutiveInitial
);

registerInitialSlice(PERMUTIVE_INITIAL_SLICE, 11);
