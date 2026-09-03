// Permission state the edge injects into the page, and the promise page code
// awaits so it can read that state whichever order the injection arrives in.
import { log } from './log';
import type { PermissionsSnapshot, TsjsApi } from './types';

function isSnapshot(value: unknown): value is PermissionsSnapshot {
  return typeof value === 'object' && value !== null;
}

/**
 * Install the `permissions` accessor and `whenPermissions()` on the API object.
 *
 * The edge injects `window.tsjs.permissions` either before this bundle runs
 * (inline mode, at head open) or after it has initialized (shared-template
 * mode, as a plain assignment at the `</body>` seam), and on a page with no
 * seam it never arrives at all. An accessor observes the plain assignment, so
 * the promise resolves in every one of those orders.
 */
export function installPermissions(api: TsjsApi): void {
  // A value already on the API object came from the head-open injection, so it
  // is the current value; otherwise page code must still read a defined value.
  const injected = api.permissions;
  let current: PermissionsSnapshot = isSnapshot(injected) ? injected : { set: [] };
  let settled = false;
  let resolvePending: (snapshot: PermissionsSnapshot) => void = () => {};
  const pending = new Promise<PermissionsSnapshot>((resolve) => {
    resolvePending = resolve;
  });

  function settle(snapshot: PermissionsSnapshot): void {
    if (settled) return;
    settled = true;
    resolvePending(snapshot);
  }

  Object.defineProperty(api, 'permissions', {
    get(): PermissionsSnapshot {
      return current;
    },
    set(value: PermissionsSnapshot) {
      current = value;
      log.debug('permissions: received', value);
      settle(value);
    },
    enumerable: true,
    configurable: true,
  });

  if (isSnapshot(injected)) {
    log.debug('permissions: present at initialization', injected);
    settle(current);
  } else if (typeof document !== 'undefined') {
    // The seam sits at `</body>`, so anything it was going to assign has run by
    // the time the document is parsed. Resolve with whatever the current value
    // is rather than leaving page code waiting on a page that has no seam. A
    // bundle that initializes after parsing has already missed any seam, so it
    // resolves at once instead of waiting for an event that will never fire.
    if (document.readyState === 'loading') {
      document.addEventListener(
        'DOMContentLoaded',
        () => {
          settle(current);
        },
        { once: true }
      );
    } else {
      settle(current);
    }
  }

  api.whenPermissions = () => pending;
}
