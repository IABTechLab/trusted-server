import { defineInitialSlice, registerInitialSlice } from './definition';

export const PERMUTIVE_INITIAL_SLICE = defineInitialSlice('permutive_initial');

registerInitialSlice(PERMUTIVE_INITIAL_SLICE, 10);
