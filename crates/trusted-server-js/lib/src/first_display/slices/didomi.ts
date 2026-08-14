import { defineInitialSlice, registerInitialSlice } from './definition';

export const DIDOMI_INITIAL_SLICE = defineInitialSlice('didomi_initial');

registerInitialSlice(DIDOMI_INITIAL_SLICE, 5);
