import { installSourcepointInitial } from '../leaf/consent_snapshot';

import { defineInitialSlice, registerInitialSlice } from './definition';

export const SOURCEPOINT_INITIAL_SLICE = defineInitialSlice(
  'sourcepoint_initial',
  installSourcepointInitial
);

registerInitialSlice(SOURCEPOINT_INITIAL_SLICE, 11);
