import { describe, it, expect, beforeEach, vi } from 'vitest';

import type { AdUnit } from '../../src/core/types';

describe('registry', () => {
  beforeEach(async () => {
    await vi.resetModules();
  });

  it('adds ad units and returns size', async () => {
    const { addAdUnits, firstSize, getAllUnits } = await import('../../src/core/registry');
    const unit = {
      code: 'u1',
      mediaTypes: {
        banner: {
          sizes: [
            [320, 50],
            [300, 250],
          ],
        },
      },
    } as AdUnit;
    addAdUnits(unit);

    const all = getAllUnits();
    expect(all.length).toBe(1);
    expect(firstSize(all[0])!.join('x')).toBe('320x50');
  });
});
