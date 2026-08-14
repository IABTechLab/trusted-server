import { describe, expect, it, vi } from 'vitest';

import {
  INITIAL_SLICE_DEFINITIONS,
  selectInitialSliceDefinitions,
} from '../../src/first_display/composition';

describe('first-display initial slice definitions', () => {
  it('pins the exact twelve optional slices in build order', () => {
    expect(INITIAL_SLICE_DEFINITIONS.map(({ id }) => id)).toEqual([
      'aps_initial',
      'creative_initial',
      'datadome_initial',
      'didomi_initial',
      'google_tag_manager_initial',
      'gpt_initial',
      'lockr_initial',
      'osano_initial',
      'permutive_initial',
      'sourcepoint_initial',
      'prebid_initial',
      'testlight_initial',
    ]);
  });

  it('prepares inertly and activates only its exact host obligation', () => {
    const events: string[] = [];
    const dispose = vi.fn();
    const host = Object.freeze({
      activate: (id: string, own: (callback: () => void) => void) => {
        own(dispose);
        events.push(id);
      },
    });

    const prepared = INITIAL_SLICE_DEFINITIONS.map((definition) => definition.prepare(host));
    expect(events).toEqual([]);
    for (const slice of prepared) slice.activate(Object.freeze({ own: () => undefined }));
    expect(events).toEqual(INITIAL_SLICE_DEFINITIONS.map(({ id }) => id));
    expect(
      INITIAL_SLICE_DEFINITIONS.every(
        (definition) => Reflect.ownKeys(definition).join(',') === 'id,prepare'
      )
    ).toBe(true);
  });

  it('rejects unknown, duplicate, omitted-base, and misordered selections', () => {
    expect(selectInitialSliceDefinitions(['first_display', 'gpt_initial'])?.map(({ id }) => id)).toEqual([
      'gpt_initial',
    ]);
    expect(selectInitialSliceDefinitions(['gpt_initial'])).toBeUndefined();
    expect(selectInitialSliceDefinitions(['first_display', 'gpt_initial', 'gpt_initial'])).toBeUndefined();
    expect(selectInitialSliceDefinitions(['first_display', 'prebid_initial', 'gpt_initial'])).toBeUndefined();
    expect(
      selectInitialSliceDefinitions(['first_display', 'unknown_initial' as 'gpt_initial'])
    ).toBeUndefined();
  });
});
