import type { FirstDisplaySliceId } from '../kernel/release_catalog';

import { APS_INITIAL_SLICE } from './slices/aps';
import { CREATIVE_INITIAL_SLICE } from './slices/creative';
import { DATADOME_INITIAL_SLICE } from './slices/datadome';
import { DIDOMI_INITIAL_SLICE } from './slices/didomi';
import type { InitialSliceDefinition } from './slices/definition';
import { GOOGLE_TAG_MANAGER_INITIAL_SLICE } from './slices/google_tag_manager';
import { GPT_INITIAL_SLICE } from './slices/gpt';
import { LOCKR_INITIAL_SLICE } from './slices/lockr';
import { OSANO_INITIAL_SLICE } from './slices/osano';
import { PERMUTIVE_INITIAL_SLICE } from './slices/permutive';
import { PREBID_INITIAL_SLICE } from './slices/prebid';
import { RENDER_OWNER_INITIAL_SLICE } from './slices/render_owner';
import { SOURCEPOINT_INITIAL_SLICE } from './slices/sourcepoint';
import { TESTLIGHT_INITIAL_SLICE } from './slices/testlight';

export const INITIAL_SLICE_DEFINITIONS: readonly InitialSliceDefinition[] = Object.freeze([
  RENDER_OWNER_INITIAL_SLICE,
  APS_INITIAL_SLICE,
  CREATIVE_INITIAL_SLICE,
  DATADOME_INITIAL_SLICE,
  DIDOMI_INITIAL_SLICE,
  GOOGLE_TAG_MANAGER_INITIAL_SLICE,
  GPT_INITIAL_SLICE,
  LOCKR_INITIAL_SLICE,
  OSANO_INITIAL_SLICE,
  PERMUTIVE_INITIAL_SLICE,
  SOURCEPOINT_INITIAL_SLICE,
  PREBID_INITIAL_SLICE,
  TESTLIGHT_INITIAL_SLICE,
]);

/** Resolve only the canonical optional slice definitions already selected by the server. */
export function selectInitialSliceDefinitions(
  selected: readonly FirstDisplaySliceId[]
): readonly InitialSliceDefinition[] | undefined {
  if (selected[0] !== 'first_display' || selected.length > INITIAL_SLICE_DEFINITIONS.length + 1) {
    return undefined;
  }
  const requested = new Set(selected.slice(1));
  if (requested.size !== selected.length - 1) return undefined;
  if (
    (requested.has('aps_initial') && !requested.has('render_owner_initial')) ||
    (requested.has('render_owner_initial') && !requested.has('gpt_initial'))
  )
    return undefined;
  const definitions = INITIAL_SLICE_DEFINITIONS.filter(({ id }) => requested.has(id));
  if (definitions.length !== requested.size) return undefined;
  const canonical = ['first_display', ...definitions.map(({ id }) => id)];
  if (canonical.some((id, index) => selected[index] !== id)) return undefined;
  return Object.freeze(definitions);
}
