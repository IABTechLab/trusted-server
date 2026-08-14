import { installOsanoInitial } from '../leaf/consent_snapshot';

import { defineInitialSlice, registerInitialSlice } from './definition';

export const OSANO_INITIAL_SLICE = defineInitialSlice('osano_initial', installOsanoInitial);

registerInitialSlice(OSANO_INITIAL_SLICE, 9);
