import type { AdUnit, Size } from './types'
import { toArray } from './util'
import { log } from './log'

const registry = new Map<string, AdUnit>()

export function addAdUnits(units: AdUnit | AdUnit[]): void {
  for (const u of toArray(units)) {
    if (!u || !u.code) continue
    registry.set(u.code, { ...registry.get(u.code), ...u })
  }
  log.info('addAdUnits:', { count: toArray(units).length })
}

export function firstSize(unit: AdUnit): Size | null {
  const sizes = unit.mediaTypes?.banner?.sizes
  return sizes && sizes.length ? sizes[0] : null
}

export function getAllUnits(): AdUnit[] {
  return Array.from(registry.values())
}

export function getUnit(code: string): AdUnit | undefined {
  return registry.get(code)
}

export function getAllCodes(): string[] {
  return Array.from(registry.keys())
}
