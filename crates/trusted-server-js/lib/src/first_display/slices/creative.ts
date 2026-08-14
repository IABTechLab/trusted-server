import { defineInitialSlice, registerInitialSlice } from './definition';

export const CREATIVE_INITIAL_SLICE = defineInitialSlice('creative_initial');

registerInitialSlice(CREATIVE_INITIAL_SLICE, 3);
