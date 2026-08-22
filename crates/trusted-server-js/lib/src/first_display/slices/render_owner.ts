import { installRenderOwnerInitial } from '../render_journal';

import { defineInitialSlice, registerInitialSlice } from './definition';

export const RENDER_OWNER_INITIAL_SLICE = defineInitialSlice(
  'render_owner_initial',
  installRenderOwnerInitial
);

registerInitialSlice(RENDER_OWNER_INITIAL_SLICE, 2);
