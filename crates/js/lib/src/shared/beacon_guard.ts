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

/**
 * Create an independent beacon guard for a specific integration.
 */
export function createBeaconGuard(config: BeaconGuardConfig): BeaconGuard {
  let installed = false;
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
      const originalSendBeacon = navigator.sendBeacon.bind(navigator);

      navigator.sendBeacon = function (url: string, data?: BodyInit | null): boolean {
        if (config.isTargetUrl(url)) {
          const rewritten = config.rewriteUrl(url);
          log.info(`${prefix}: rewriting sendBeacon`, { original: url, rewritten });
          return originalSendBeacon(rewritten, data);
        }
        return originalSendBeacon(url, data);
      };
    }

    // --- Patch window.fetch ---
    if (typeof window.fetch === 'function') {
      const originalFetch = window.fetch.bind(window);

      window.fetch = function (input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
        const url = extractUrl(input);

        if (url && config.isTargetUrl(url)) {
          const rewritten = config.rewriteUrl(url);
          log.info(`${prefix}: rewriting fetch`, { original: url, rewritten });

          // If the input was a Request, create a new one with the rewritten URL
          if (input instanceof Request) {
            const newRequest = new Request(rewritten, input);
            return originalFetch(newRequest, init);
          }
          return originalFetch(rewritten, init);
        }

        return originalFetch(input, init);
      };
    }

    installed = true;
    log.info(`${prefix}: network interception installed successfully`);
  }

  function isInstalled(): boolean {
    return installed;
  }

  function reset(): void {
    installed = false;
  }

  return { install, isInstalled, reset };
}
