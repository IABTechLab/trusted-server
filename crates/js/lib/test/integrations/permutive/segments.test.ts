import { describe, it, expect, beforeEach, vi, afterEach } from 'vitest';

describe('getPermutiveSegments', () => {
  let getPermutiveSegments: () => string[];

  beforeEach(async () => {
    await vi.resetModules();
    localStorage.clear();
    const mod = await import('../../../src/integrations/permutive/segments');
    getPermutiveSegments = mod.getPermutiveSegments;
  });

  afterEach(() => {
    localStorage.clear();
  });

  it('returns empty array when no permutive-app in localStorage', () => {
    expect(getPermutiveSegments()).toEqual([]);
  });

  it('returns empty array when permutive-app is invalid JSON', () => {
    localStorage.setItem('permutive-app', 'not-json');
    expect(getPermutiveSegments()).toEqual([]);
  });

  it('reads segments from core.cohorts.all (primary path)', () => {
    localStorage.setItem(
      'permutive-app',
      JSON.stringify({
        core: { cohorts: { all: ['10000001', '10000003', 'adv', 'bhgp'] } },
      })
    );
    expect(getPermutiveSegments()).toEqual(['10000001', '10000003', 'adv', 'bhgp']);
  });

  it('converts numeric cohort IDs to strings', () => {
    localStorage.setItem(
      'permutive-app',
      JSON.stringify({
        core: { cohorts: { all: [123, 456] } },
      })
    );
    expect(getPermutiveSegments()).toEqual(['123', '456']);
  });

  it('falls back to eventUpload when cohorts.all is missing', () => {
    localStorage.setItem(
      'permutive-app',
      JSON.stringify({
        eventPublication: {
          eventUpload: [['key1', { event: { properties: { segments: ['seg1', 'seg2'] } } }]],
        },
      })
    );
    expect(getPermutiveSegments()).toEqual(['seg1', 'seg2']);
  });

  it('reads most recent eventUpload entry first', () => {
    localStorage.setItem(
      'permutive-app',
      JSON.stringify({
        eventPublication: {
          eventUpload: [
            ['old', { event: { properties: { segments: ['old1'] } } }],
            ['new', { event: { properties: { segments: ['new1', 'new2'] } } }],
          ],
        },
      })
    );
    // Should return the last (most recent) entry
    expect(getPermutiveSegments()).toEqual(['new1', 'new2']);
  });

  it('returns empty array when cohorts.all is empty and no eventUpload', () => {
    localStorage.setItem(
      'permutive-app',
      JSON.stringify({
        core: { cohorts: { all: [] } },
      })
    );
    expect(getPermutiveSegments()).toEqual([]);
  });

  it('caps segments at 100', () => {
    const ids = Array.from({ length: 150 }, (_, i) => `seg-${i}`);
    localStorage.setItem('permutive-app', JSON.stringify({ core: { cohorts: { all: ids } } }));
    const result = getPermutiveSegments();
    expect(result).toHaveLength(100);
    expect(result[0]).toBe('seg-0');
    expect(result[99]).toBe('seg-99');
  });

  it('filters out non-string non-number values', () => {
    localStorage.setItem(
      'permutive-app',
      JSON.stringify({
        core: { cohorts: { all: ['valid', 123, null, undefined, true, { obj: 1 }] } },
      })
    );
    expect(getPermutiveSegments()).toEqual(['valid', '123']);
  });
});
