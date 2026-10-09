import { describe, expect, it } from 'vitest';

import { validDiagnosticAuctionId, validTraceSlotRef } from '../../src/trace/validation';

export const AUCTION = 'ts-auc-1234567812344abc8def123456789abc';
export const SLOT = 'ts-slot-12345678-1234-4abc-8def-123456789abc';

describe('exact opaque trace tokens', () => {
  it('accepts only the producer-specific UUID v4 formats', () => {
    expect(validDiagnosticAuctionId(AUCTION)).toBe(true);
    expect(validTraceSlotRef(SLOT)).toBe(true);
  });

  it.each([
    null,
    1,
    '',
    'internal-auction-id',
    AUCTION.toUpperCase(),
    ` ${AUCTION}`,
    `${AUCTION} `,
    AUCTION.replace('4abc', '5abc'),
    AUCTION.replace('8def', '7def'),
    AUCTION.replace('8def', 'cdef'),
    'ts-auc-12345678-1234-4abc-8def-123456789abc',
    `${AUCTION}\n`,
    `${AUCTION}\u202e`,
    SLOT,
  ])('rejects every alternate auction-token spelling %j', (value) => {
    expect(validDiagnosticAuctionId(value)).toBe(false);
  });

  it.each([
    null,
    1,
    '',
    'raw-slot-element-id',
    SLOT.toUpperCase(),
    ` ${SLOT}`,
    `${SLOT} `,
    SLOT.replace('4abc', '3abc'),
    SLOT.replace('8def', '7def'),
    SLOT.replace('8def', 'cdef'),
    'ts-slot-1234567812344abc8def123456789abc',
    `${SLOT}\n`,
    `${SLOT}\u202e`,
    AUCTION,
  ])('rejects every alternate slot-token spelling %j', (value) => {
    expect(validTraceSlotRef(value)).toBe(false);
  });
});
