import { describe, it, expect, beforeEach, vi } from 'vitest';

import type { AdUnit } from '../../src/core/types';
import {
  AdUnitRegistrationError,
  prepareProgrammaticAdUnits,
  serializeAuctionRequestBody,
} from '../../src/core/registry';

function unit(code = 'programmatic-slot'): Record<string, unknown> {
  return {
    code,
    mediaTypes: { banner: { sizes: [[300, 250]] } },
    bids: [{ bidder: 'fictional', params: { placement: 7 } }],
  };
}

function expectRegistrationError(
  callback: () => unknown,
  code: AdUnitRegistrationError['code'],
  unitIndex?: number
): void {
  try {
    callback();
    throw new Error('should reject registration');
  } catch (error) {
    expect(error).toBeInstanceOf(AdUnitRegistrationError);
    expect(error).toMatchObject({ code, ...(unitIndex === undefined ? {} : { unitIndex }) });
  }
}

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
    expect(firstSize(all[0]!)!.join('x')).toBe('320x50');
  });

  it('detaches and recursively freezes one or many exact programmatic units', () => {
    const first = unit('first');
    const second = unit('second');
    const prepared = prepareProgrammaticAdUnits([first, second], new Set(['server-slot']));

    expect(prepared.map(({ code }) => code)).toEqual(['first', 'second']);
    expect(Object.isFrozen(prepared)).toBe(true);
    expect(Object.isFrozen(prepared[0])).toBe(true);
    expect(Object.isFrozen(prepared[0]?.mediaTypes.banner.sizes)).toBe(true);
    expect(Object.isFrozen(prepared[0]?.bids?.[0]?.params)).toBe(true);
    (
      (first.bids as Array<{ params: { placement: number } }>)[0]!.params as { placement: number }
    ).placement = 99;
    expect(prepared[0]?.bids?.[0]?.params).toEqual({ placement: 7 });

    expect(prepareProgrammaticAdUnits(unit('single'), new Set())).toHaveLength(1);
  });

  it.each([
    [null, 'invalid_unit', 0],
    [[], 'invalid_units', undefined],
    [Array.from({ length: 257 }, (_, index) => unit(`slot-${index}`)), 'invalid_units', undefined],
    [{ ...unit(), unknown: true }, 'invalid_unit', 0],
    [{ code: '', mediaTypes: { banner: { sizes: [[300, 250]] } } }, 'invalid_code', 0],
    [[unit('same'), unit('same')], 'duplicate_code', 1],
    [unit('occupied'), 'slot_collision', 0],
    [{ code: 'slot', mediaTypes: {} }, 'invalid_media_types', 0],
    [{ code: 'slot', mediaTypes: { banner: { sizes: [] } } }, 'invalid_media_types', 0],
    [{ code: 'slot', mediaTypes: { banner: { sizes: [[0, 250]] } } }, 'invalid_dimensions', 0],
    [{ code: 'slot', mediaTypes: { banner: { sizes: [[1.5, 250]] } } }, 'invalid_dimensions', 0],
    [
      { code: 'slot', mediaTypes: { banner: { sizes: [[4_097, 250]] } } },
      'dimensions_out_of_range',
      0,
    ],
    [{ ...unit(), bids: null }, 'invalid_bids', 0],
    [{ ...unit(), bids: [{ bidder: '' }] }, 'invalid_bidder', 0],
    [{ ...unit(), bids: [{ bidder: 'a'.repeat(65) }] }, 'invalid_bidder', 0],
    [{ ...unit(), bids: [{ bidder: 'fictional', params: [] }] }, 'invalid_params', 0],
  ] as const)('rejects invalid registration %# with the exact code', (candidate, code, index) => {
    const occupied = new Set(candidate === null ? [] : ['occupied']);
    expectRegistrationError(() => prepareProgrammaticAdUnits(candidate, occupied), code, index);
  });

  it('rejects accessors, foreign prototypes, cyclic params, and oversized bodies without reads', () => {
    const getter = vi.fn(() => 'accessed');
    const accessor = unit();
    Object.defineProperty(accessor, 'code', { enumerable: true, get: getter });
    expectRegistrationError(
      () => prepareProgrammaticAdUnits(accessor, new Set()),
      'invalid_unit',
      0
    );
    expect(getter).not.toHaveBeenCalled();

    const foreign = Object.assign(Object.create({ inherited: true }), unit());
    expectRegistrationError(
      () => prepareProgrammaticAdUnits(foreign, new Set()),
      'invalid_unit',
      0
    );

    const cyclic: Record<string, unknown> = {};
    cyclic.self = cyclic;
    expectRegistrationError(
      () =>
        prepareProgrammaticAdUnits(
          { ...unit(), bids: [{ bidder: 'fictional', params: cyclic }] },
          new Set()
        ),
      'invalid_params',
      0
    );

    expectRegistrationError(
      () =>
        prepareProgrammaticAdUnits(
          {
            ...unit(),
            bids: [{ bidder: 'fictional', params: { payload: 'x'.repeat(256 * 1024) } }],
          },
          new Set()
        ),
      'request_body_too_large'
    );
  });

  it('accepts exact bidder and dimension boundaries and enforces combined capacity last', () => {
    for (const bidderLength of [63, 64]) {
      expect(
        prepareProgrammaticAdUnits(
          { ...unit(), bids: [{ bidder: 'a'.repeat(bidderLength), params: {} }] },
          new Set()
        )
      ).toHaveLength(1);
    }
    for (const dimension of [1, 4_096]) {
      expect(
        prepareProgrammaticAdUnits(
          {
            code: `slot-${dimension}`,
            mediaTypes: { banner: { sizes: [[dimension, dimension]] } },
          },
          new Set()
        )
      ).toHaveLength(1);
    }
    for (const existingCount of [254, 255]) {
      const existing = new Set(
        Array.from({ length: existingCount }, (_, index) => `server-${index}`)
      );
      expect(prepareProgrammaticAdUnits(unit(`at-${existingCount + 1}`), existing)).toHaveLength(1);
    }
    const existing = new Set(Array.from({ length: 256 }, (_, index) => `server-${index}`));
    expectRegistrationError(
      () => prepareProgrammaticAdUnits(unit('overflow'), existing),
      'registry_capacity'
    );
  });

  it('enforces the encoded auction-unit body cap at the exact byte boundary', () => {
    const candidate = {
      code: 'body-boundary',
      mediaTypes: { banner: { sizes: [[300, 250]] } },
      bids: [{ bidder: 'fictional', params: { payload: '' } }],
    };
    const baseBytes = new TextEncoder().encode(
      JSON.stringify({ adUnits: [candidate], config: {} })
    ).byteLength;
    const payloadAtLimit = 'x'.repeat(256 * 1024 - baseBytes);
    candidate.bids[0]!.params.payload = payloadAtLimit;
    expect(
      new TextEncoder().encode(JSON.stringify({ adUnits: [candidate], config: {} }))
    ).toHaveLength(256 * 1024);
    expect(prepareProgrammaticAdUnits(candidate, new Set())).toHaveLength(1);

    candidate.bids[0]!.params.payload += 'x';
    expectRegistrationError(
      () => prepareProgrammaticAdUnits(candidate, new Set()),
      'request_body_too_large'
    );
  });

  it('serializes detached auction data without invoking inherited toJSON hooks', () => {
    const prepared = prepareProgrammaticAdUnits(unit(), new Set());
    const context = Object.freeze({ segments: Object.freeze(['one']) });
    const publisherHook = vi.fn(() => {
      throw new Error('publisher toJSON hook');
    });
    Object.defineProperty(Object.prototype, 'toJSON', {
      configurable: true,
      value: publisherHook,
    });
    Object.defineProperty(Array.prototype, 'toJSON', {
      configurable: true,
      value: publisherHook,
    });
    let body: string | undefined;
    try {
      body = serializeAuctionRequestBody(prepared, context);
    } finally {
      Reflect.deleteProperty(Object.prototype, 'toJSON');
      Reflect.deleteProperty(Array.prototype, 'toJSON');
    }

    expect(publisherHook).not.toHaveBeenCalled();
    expect(body).toBe(JSON.stringify({ adUnits: prepared, config: context }));
  });
});
