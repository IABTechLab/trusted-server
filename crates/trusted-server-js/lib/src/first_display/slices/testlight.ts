import { defineInitialSlice, registerInitialSlice } from './definition';

export const TESTLIGHT_INITIAL_SLICE = defineInitialSlice('testlight_initial');

registerInitialSlice(TESTLIGHT_INITIAL_SLICE, 13);
