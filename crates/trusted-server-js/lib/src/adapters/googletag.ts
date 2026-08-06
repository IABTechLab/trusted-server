/** The live state of the publisher-owned `window.googletag` binding. */
export type GoogletagBindingStatus = 'present' | 'pending' | 'incompatible';

/** Narrow GPT boundary consumed by kernel sessions and services. */
export interface GoogletagAdapter {
  bindingStatus(): GoogletagBindingStatus;
}

/** Browser surface owned by the concrete GPT adapter. */
export interface GoogletagGlobalTarget {
  googletag?: unknown;
}

function bindingStatus(value: unknown): GoogletagBindingStatus {
  if (value === undefined || value === null) return 'pending';
  return typeof value === 'object' || typeof value === 'function' ? 'present' : 'incompatible';
}

/** Create the sole production reader/writer boundary for `window.googletag`. */
export function createBrowserGoogletagAdapter(
  target: GoogletagGlobalTarget = window as unknown as GoogletagGlobalTarget
): GoogletagAdapter {
  return Object.freeze({
    bindingStatus: () => bindingStatus(target.googletag),
  });
}

/** Create a side-effect-free GPT boundary for tests and unavailable environments. */
export function createNoopGoogletagAdapter(): GoogletagAdapter {
  return Object.freeze({
    bindingStatus: () => 'pending',
  });
}
