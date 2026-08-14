import { defineInitialSlice, registerInitialSlice } from './definition';

export const DATADOME_INITIAL_SLICE = defineInitialSlice('datadome_initial');

registerInitialSlice(DATADOME_INITIAL_SLICE, 4);
