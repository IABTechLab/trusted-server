import { defineInitialSlice, registerInitialSlice } from './definition';

export const LOCKR_INITIAL_SLICE = defineInitialSlice('lockr_initial');

registerInitialSlice(LOCKR_INITIAL_SLICE, 8);
