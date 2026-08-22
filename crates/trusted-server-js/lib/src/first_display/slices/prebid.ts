import { installPrebidInitial } from '../leaf/prebid_protocol';

import { defineInitialSlice, registerInitialSlice } from './definition';

export const PREBID_INITIAL_SLICE = defineInitialSlice('prebid_initial', installPrebidInitial);

registerInitialSlice(PREBID_INITIAL_SLICE, 13);
