import { defineInitialSlice, registerInitialSlice } from './definition';

export const APS_INITIAL_SLICE = defineInitialSlice('aps_initial');

registerInitialSlice(APS_INITIAL_SLICE, 2);
