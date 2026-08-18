import { installApsInitial } from '../leaf/aps_protocol';

import { defineInitialSlice, registerInitialSlice } from './definition';

export const APS_INITIAL_SLICE = defineInitialSlice('aps_initial', installApsInitial);

registerInitialSlice(APS_INITIAL_SLICE, 2);
