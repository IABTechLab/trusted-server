import { defineInitialSlice, registerInitialSlice } from './definition';

export const OSANO_INITIAL_SLICE = defineInitialSlice('osano_initial');

registerInitialSlice(OSANO_INITIAL_SLICE, 9);
