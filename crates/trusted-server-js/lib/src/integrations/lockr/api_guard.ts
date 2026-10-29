let uninstall: (() => void) | undefined;

/**
 * Keep root-relative Lockr API requests on the page origin, even with an external base URL.
 *
 * The dedicated trust-server SDK currently makes its API calls through XMLHttpRequest.
 * Only XMLHttpRequest.open is guarded. Requests made with fetch or navigator.sendBeacon
 * still resolve relative URLs against the document base URL.
 */
export function installLockrApiGuard(): void {
  if (uninstall || typeof window === 'undefined' || typeof window.XMLHttpRequest === 'undefined') {
    return;
  }

  const prototype = window.XMLHttpRequest.prototype;
  const originalOpen = prototype.open;
  const open: typeof originalOpen = function (
    this: XMLHttpRequest,
    ...args: [
      method: string,
      url: string | URL,
      async?: boolean,
      username?: string | null,
      password?: string | null,
    ]
  ): void {
    const url = args[1];
    if (typeof url === 'string' && /^\/integrations\/lockr\/api(?:[/?#]|$)/.test(url)) {
      args[1] = `${window.location.origin}${url}`;
    }
    Reflect.apply(originalOpen, this, args);
  };

  prototype.open = open;
  uninstall = () => {
    if (prototype.open === open) {
      prototype.open = originalOpen;
    }
  };
}

/** Restore the original XHR method when this guard still owns it, primarily for tests. */
export function resetApiGuardState(): void {
  uninstall?.();
  uninstall = undefined;
}
