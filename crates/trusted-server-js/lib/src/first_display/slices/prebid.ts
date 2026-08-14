import { defineInitialSlice, registerInitialSlice } from './definition';

export const PREBID_INITIAL_SLICE = defineInitialSlice('prebid_initial');

registerInitialSlice(PREBID_INITIAL_SLICE, 12);
