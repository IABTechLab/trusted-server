import type { AuctionSlot } from './types';

/**
 * The sizes a slot's viewport band asks for at the current viewport width, an
 * empty list where that band asks for none, or `undefined` when the slot names
 * no band. Bands come widest first, each from its narrowest width, so the first
 * band the width reaches is the one in effect.
 */
export function viewportBandSizes(
  slot: AuctionSlot | undefined
): Array<[number, number]> | undefined {
  const bands = slot?.viewport_sizes;
  if (!bands?.length) return undefined;
  const width = window.innerWidth;
  return bands.find(([[minimumWidth]]) => width >= minimumWidth)?.[1] ?? [];
}
