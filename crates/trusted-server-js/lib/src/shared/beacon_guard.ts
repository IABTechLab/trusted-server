import { log } from '../core/log';

/**
 * Shared Beacon Guard Factory
 *
 * Creates a network interception guard that patches `navigator.sendBeacon`
 * and `window.fetch` to intercept outgoing beacon/analytics requests whose
 * URLs match an integration's target domains. Matched URLs are rewritten to
 * a first-party proxy endpoint.
 *
 * This complements the script_guard (which intercepts DOM insertions) by
 * handling the _runtime_ network calls that analytics SDKs use to send data.
 *
 * Each call to createBeaconGuard() produces an independent guard with its
 * own installation state, so multiple integrations can coexist.
 */

export interface BeaconGuardConfig {
  /** Integration name used in log messages (e.g. "GTM"). */
  name: string;
  /** Return true if the URL belongs to this integration's analytics domain. */
  isTargetUrl: (url: string) => boolean;
  /** Rewrite the original URL to a first-party proxy URL. */
  rewriteUrl: (url: string) => string;
}

export interface BeaconGuard {
  /** Patch sendBeacon/fetch to intercept matching beacon requests. */
  install: () => void;
  /** Whether the guard has already been installed. */
  isInstalled: () => boolean;
  /** Reset installation state (primarily for testing). */
  reset: () => void;
}

/**
 * Extract a URL string from the various input types that fetch() accepts.
 * Returns null if the input can't be resolved to a URL string.
 */
function extractUrl(input: RequestInfo | URL): string | null {
  if (typeof input === 'string') {
    return input;
  }
  if (input instanceof URL) {
    return input.href;
  }
  if (input instanceof Request) {
    return input.url;
  }
  return null;
}

function sameDescriptor(
  left: PropertyDescriptor | undefined,
  right: PropertyDescriptor | undefined
): boolean {
  if (!left || !right) return left === right;
  if (left.configurable !== right.configurable || left.enumerable !== right.enumerable) {
    return false;
  }
  if ('value' in left || 'value' in right) {
    return (
      'value' in left &&
      'value' in right &&
      left.value === right.value &&
      left.writable === right.writable
    );
  }
  return left.get === right.get && left.set === right.set;
}

function restoreOwnedDescriptor(
  target: object,
  property: PropertyKey,
  installed: PropertyDescriptor | undefined,
  original: PropertyDescriptor | undefined
): void {
  try {
    if (!sameDescriptor(Object.getOwnPropertyDescriptor(target, property), installed)) return;
    if (original) Object.defineProperty(target, property, original);
    else Reflect.deleteProperty(target, property);
  } catch {
    // Publisher replacement or a hostile descriptor cannot block independent cleanup.
  }
}

/**
 * Create an independent beacon guard for a specific integration.
 */
export function createBeaconGuard(config: BeaconGuardConfig): BeaconGuard {
  let installed = false;
  let originalSendBeacon: typeof navigator.sendBeacon | undefined;
  let originalSendBeaconDescriptor: PropertyDescriptor | undefined;
  let installedSendBeaconDescriptor: PropertyDescriptor | undefined;
  let originalFetch: typeof window.fetch | undefined;
  let originalFetchDescriptor: PropertyDescriptor | undefined;
  let installedFetchDescriptor: PropertyDescriptor | undefined;
  let sendBeaconPatched = false;
  let fetchPatched = false;
  const prefix = `${config.name} beacon guard`;

  function install(): void {
    if (installed) {
      log.debug(`${prefix}: already installed, skipping`);
      return;
    }

    if (typeof window === 'undefined') {
      log.debug(`${prefix}: not in browser environment, skipping`);
      return;
    }

    log.info(`${prefix}: installing network interception`);

    // --- Patch navigator.sendBeacon ---
    if (typeof navigator !== 'undefined' && typeof navigator.sendBeacon === 'function') {
      originalSendBeacon = navigator.sendBeacon;
      originalSendBeaconDescriptor = Object.getOwnPropertyDescriptor(navigator, 'sendBeacon');
      sendBeaconPatched = true;

      const wrapper = function (url: string, data?: BodyInit | null): boolean {
        const sendBeacon = originalSendBeacon;
        if (!sendBeacon) return false;
        if (config.isTargetUrl(url)) {
          const rewritten = config.rewriteUrl(url);
          log.info(`${prefix}: rewriting sendBeacon`, { original: url, rewritten });
          return Reflect.apply(sendBeacon, navigator, [rewritten, data]);
        }
        return Reflect.apply(sendBeacon, navigator, [url, data]);
      };
      navigator.sendBeacon = wrapper;
      installedSendBeaconDescriptor = Object.getOwnPropertyDescriptor(navigator, 'sendBeacon');
      sendBeaconPatched = sameDescriptor(installedSendBeaconDescriptor, {
        ...installedSendBeaconDescriptor,
        value: wrapper,
      });
    }

    // --- Patch window.fetch ---
    if (typeof window.fetch === 'function') {
      originalFetch = window.fetch;
      originalFetchDescriptor = Object.getOwnPropertyDescriptor(window, 'fetch');
      fetchPatched = true;

      const wrapper = function (input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const fetch = originalFetch;
        if (!fetch) return Promise.reject(new TypeError('fetch is unavailable'));
        const url = extractUrl(input);

        if (url && config.isTargetUrl(url)) {
          const rewritten = config.rewriteUrl(url);
          log.info(`${prefix}: rewriting fetch`, { original: url, rewritten });

          // If the input was a Request, create a new one with the rewritten URL
          if (input instanceof Request) {
            const newRequest = new Request(rewritten, input);
            return Reflect.apply(fetch, window, [newRequest, init]);
          }
          return Reflect.apply(fetch, window, [rewritten, init]);
        }

        return Reflect.apply(fetch, window, [input, init]);
      };
      window.fetch = wrapper;
      installedFetchDescriptor = Object.getOwnPropertyDescriptor(window, 'fetch');
      fetchPatched = sameDescriptor(installedFetchDescriptor, {
        ...installedFetchDescriptor,
        value: wrapper,
      });
    }

    installed = true;
    log.info(`${prefix}: network interception installed successfully`);
  }

  function isInstalled(): boolean {
    return installed;
  }

  function reset(): void {
    if (sendBeaconPatched && typeof navigator !== 'undefined') {
      restoreOwnedDescriptor(
        navigator,
        'sendBeacon',
        installedSendBeaconDescriptor,
        originalSendBeaconDescriptor
      );
    }
    if (fetchPatched && typeof window !== 'undefined') {
      restoreOwnedDescriptor(window, 'fetch', installedFetchDescriptor, originalFetchDescriptor);
    }
    originalSendBeacon = undefined;
    originalSendBeaconDescriptor = undefined;
    installedSendBeaconDescriptor = undefined;
    originalFetch = undefined;
    originalFetchDescriptor = undefined;
    installedFetchDescriptor = undefined;
    sendBeaconPatched = false;
    fetchPatched = false;
    installed = false;
    log.debug(`${prefix}: reset and uninstalled`);
  }

  return { install, isInstalled, reset };
}
