import { installDataDomeInitial } from '../leaf/route_guard';

import { defineInitialSlice, registerInitialSlice } from './definition';

export const DATADOME_INITIAL_SLICE = defineInitialSlice(
  'datadome_initial',
  installDataDomeInitial
);

registerInitialSlice(DATADOME_INITIAL_SLICE, 5);
