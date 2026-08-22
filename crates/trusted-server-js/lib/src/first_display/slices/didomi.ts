import { installDidomiInitial } from '../leaf/config_guard';

import { defineInitialSlice, registerInitialSlice } from './definition';

export const DIDOMI_INITIAL_SLICE = defineInitialSlice('didomi_initial', installDidomiInitial);

registerInitialSlice(DIDOMI_INITIAL_SLICE, 6);
