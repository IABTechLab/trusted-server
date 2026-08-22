import { installLockrInitial } from '../leaf/route_guard';

import { defineInitialSlice, registerInitialSlice } from './definition';

export const LOCKR_INITIAL_SLICE = defineInitialSlice('lockr_initial', installLockrInitial);

registerInitialSlice(LOCKR_INITIAL_SLICE, 9);
