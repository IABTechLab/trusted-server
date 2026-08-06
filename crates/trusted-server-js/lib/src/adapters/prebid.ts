/** The live state of the publisher-owned `window.pbjs` binding. */
export type PrebidBindingStatus = 'present' | 'pending' | 'incompatible';

/** Narrow Prebid boundary consumed by kernel sessions and services. */
export interface PrebidAdapter {
  bindingStatus(): PrebidBindingStatus;
}

/** Browser surface owned by the concrete Prebid adapter. */
export interface PrebidGlobalTarget {
  pbjs?: unknown;
}

function bindingStatus(value: unknown): PrebidBindingStatus {
  if (value === undefined || value === null) return 'pending';
  return typeof value === 'object' || typeof value === 'function' ? 'present' : 'incompatible';
}

/** Create the sole production reader/writer boundary for `window.pbjs`. */
export function createBrowserPrebidAdapter(
  target: PrebidGlobalTarget = window as unknown as PrebidGlobalTarget
): PrebidAdapter {
  return Object.freeze({
    bindingStatus: () => bindingStatus(target.pbjs),
  });
}

/** Create a side-effect-free Prebid boundary for tests and unavailable environments. */
export function createNoopPrebidAdapter(): PrebidAdapter {
  return Object.freeze({
    bindingStatus: () => 'pending',
  });
}
