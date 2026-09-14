/** @file In-memory ad-unit registry shared by the core bundle and extensions. */
import type { AdUnit, Size } from './types';
import { toArray } from './util';
import { log } from './log';

const registry = new Map<string, AdUnit>();

/** Merge one or more ad-unit definitions by code, preserving unspecified prior fields. */
export function addAdUnits(units: AdUnit | AdUnit[]): void {
  for (const u of toArray(units)) {
    if (!u || !u.code) continue;
    registry.set(u.code, { ...registry.get(u.code), ...u });
  }
  log.info('addAdUnits:', { count: toArray(units).length });
}

/** Return the first configured banner size, or `null` when no banner size exists. */
export function firstSize(unit: AdUnit): Size | null {
  const sizes = unit.mediaTypes?.banner?.sizes;
  return sizes && sizes.length ? sizes[0] : null;
}

/** Return a snapshot of the currently registered ad units. */
export function getAllUnits(): AdUnit[] {
  return Array.from(registry.values());
}

/** Look up the current ad-unit definition for an exact code. */
export function getUnit(code: string): AdUnit | undefined {
  return registry.get(code);
}
