import { installGoogleTagManagerInitial } from '../leaf/route_guard';

import { defineInitialSlice, registerInitialSlice } from './definition';

export const GOOGLE_TAG_MANAGER_INITIAL_SLICE = defineInitialSlice(
  'google_tag_manager_initial',
  installGoogleTagManagerInitial
);

registerInitialSlice(GOOGLE_TAG_MANAGER_INITIAL_SLICE, 6);
