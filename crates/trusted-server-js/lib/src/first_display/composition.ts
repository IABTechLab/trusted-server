import type { FirstDisplaySliceId } from '../kernel/release_catalog';

import { installApsInitial } from './leaf/aps_protocol';
import { installTestlightInitial } from './leaf/callback_capture';
import { installDidomiInitial } from './leaf/config_guard';
import { installOsanoInitial, installSourcepointInitial } from './leaf/consent_snapshot';
import { installPermutiveInitial } from './leaf/context_snapshot';
import { installCreativeInitial } from './leaf/creative_guard';
import { installPrebidInitial } from './leaf/prebid_protocol';
import {
  installDataDomeInitial,
  installGoogleTagManagerInitial,
  installLockrInitial,
} from './leaf/route_guard';
import { installRenderOwnerInitial } from './render_journal';
import type { InitialSliceDefinition } from './slices/definition';
import { installGptInitialSlice } from './slices/gpt';

export const RENDER_OWNER_INITIAL_SLICE: InitialSliceDefinition = Object.freeze({
  id: 'render_owner_initial',
  install: installRenderOwnerInitial,
});
export const APS_INITIAL_SLICE: InitialSliceDefinition = Object.freeze({
  id: 'aps_initial',
  install: installApsInitial,
});
export const CREATIVE_INITIAL_SLICE: InitialSliceDefinition = Object.freeze({
  id: 'creative_initial',
  install: installCreativeInitial,
});
export const DATADOME_INITIAL_SLICE: InitialSliceDefinition = Object.freeze({
  id: 'datadome_initial',
  install: installDataDomeInitial,
});
export const DIDOMI_INITIAL_SLICE: InitialSliceDefinition = Object.freeze({
  id: 'didomi_initial',
  install: installDidomiInitial,
});
export const GOOGLE_TAG_MANAGER_INITIAL_SLICE: InitialSliceDefinition = Object.freeze({
  id: 'google_tag_manager_initial',
  install: installGoogleTagManagerInitial,
});
export const GPT_INITIAL_SLICE: InitialSliceDefinition = Object.freeze({
  id: 'gpt_initial',
  install: installGptInitialSlice,
});
export const LOCKR_INITIAL_SLICE: InitialSliceDefinition = Object.freeze({
  id: 'lockr_initial',
  install: installLockrInitial,
});
export const OSANO_INITIAL_SLICE: InitialSliceDefinition = Object.freeze({
  id: 'osano_initial',
  install: installOsanoInitial,
});
export const PERMUTIVE_INITIAL_SLICE: InitialSliceDefinition = Object.freeze({
  id: 'permutive_initial',
  install: installPermutiveInitial,
});
export const SOURCEPOINT_INITIAL_SLICE: InitialSliceDefinition = Object.freeze({
  id: 'sourcepoint_initial',
  install: installSourcepointInitial,
});
export const PREBID_INITIAL_SLICE: InitialSliceDefinition = Object.freeze({
  id: 'prebid_initial',
  install: installPrebidInitial,
});
export const TESTLIGHT_INITIAL_SLICE: InitialSliceDefinition = Object.freeze({
  id: 'testlight_initial',
  install: installTestlightInitial,
});

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
