// In-memory registry for ad units registered via tsjs (used by core + extensions).
import type { AdUnit, Size } from './types';
import { toArray } from './util';
import { log } from './log';

const registry = new Map<string, AdUnit>();

// Merge ad unit definitions into the in-memory registry (supports array or single unit).
export function addAdUnits(units: AdUnit | AdUnit[]): void {
  for (const u of toArray(units)) {
    if (!u || !u.code) continue;
    registry.set(u.code, { ...registry.get(u.code), ...u });
  }
  log.info('addAdUnits:', { count: toArray(units).length });
}

// Convenience helper to grab the first banner size off an ad unit.
export function firstSize(unit: AdUnit): Size | null {
  const sizes = unit.mediaTypes?.banner?.sizes;
  return sizes && sizes.length ? sizes[0] : null;
}

// Return a snapshot array of all registered ad units.
export function getAllUnits(): AdUnit[] {
  return Array.from(registry.values());
}

// Look up a unit by its code.
export function getUnit(code: string): AdUnit | undefined {
  return registry.get(code);
}

// Extract just the ad unit codes for quick iteration.
export function getAllCodes(): string[] {
  return Array.from(registry.keys());
}
