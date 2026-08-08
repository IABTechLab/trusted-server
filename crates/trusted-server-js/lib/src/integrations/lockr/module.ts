import type { IntegrationRegistration } from '../../kernel/integration_registry';
import {
  createLifecycleIntegrationRegistration,
  type IntegrationLifecycleRuntime,
} from '../../kernel/lifecycle_module';
import { log } from '../../core/log';

import { installLockrGuard, resetGuardState } from './script_guard';

export const LOCKR_INTEGRATION_ID = 'lockr' as const;

interface LockrSdk {
  host: string;
}

export interface LockrRuntimeDependencies {
  readonly clearTimeout: (timer: number) => void;
  readonly getSdk: () => LockrSdk | undefined;
  readonly installGuard: () => void;
  readonly location: { readonly host: string; readonly protocol: string };
  readonly resetGuard: () => void;
  readonly setTimeout: (callback: () => void, delay: number) => number;
  readonly started: () => void;
  readonly timedOut: () => void;
}

function bestEffort(action: () => void): void {
  try {
    action();
  } catch {
    // Cleanup is isolated so one hostile publisher hook cannot retain another resource.
  }
}

/** Own the Lockr guard, bounded SDK readiness timer, and installed API host. */
export function createLockrRuntime(
  dependencies: LockrRuntimeDependencies = {
    clearTimeout: (timer) => window.clearTimeout(timer),
    getSdk: () => (globalThis as typeof globalThis & { identityLockr?: LockrSdk }).identityLockr,
    installGuard: installLockrGuard,
    location: window.location,
    resetGuard: resetGuardState,
    setTimeout: (callback, delay) => window.setTimeout(callback, delay),
    started: () => log.info('Lockr integration initialized'),
    timedOut: () => log.warn('Lockr SDK not detected after', 2_500, 'ms'),
  }
): IntegrationLifecycleRuntime {
  let active = false;
  let started = false;
  let timer: number | undefined;
  let installedHost: string | undefined;
  let previousHost: string | undefined;
  let ownedSdk: LockrSdk | undefined;

  const resetSdk = (): void => {
    const sdk = ownedSdk;
    const installed = installedHost;
    const previous = previousHost;
    ownedSdk = undefined;
    installedHost = undefined;
    previousHost = undefined;
    if (!sdk || installed === undefined || previous === undefined) return;
    try {
      if (sdk.host === installed) sdk.host = previous;
    } catch {
      // Publisher replacement wins over cleanup.
    }
  };

  return Object.freeze({
    activate: (_config: unknown): (() => void) => {
      if (active) throw new Error('Lockr runtime is already active');
      try {
        dependencies.installGuard();
      } catch (error) {
        try {
          dependencies.resetGuard();
        } catch {
          // Preserve the activation failure after best-effort rollback.
        }
        throw error;
      }
      active = true;
      return (): void => {
        if (!active) return;
        active = false;
        started = false;
        if (timer !== undefined) {
          const ownedTimer = timer;
          timer = undefined;
          bestEffort(() => dependencies.clearTimeout(ownedTimer));
        }
        bestEffort(resetSdk);
        bestEffort(dependencies.resetGuard);
      };
    },
    start: (_config: unknown): void => {
      if (!active || started) return;
      started = true;
      dependencies.started();
      let attempts = 0;
      const check = (): void => {
        timer = undefined;
        if (!active) return;
        attempts += 1;
        let sdk: LockrSdk | undefined;
        try {
          sdk = dependencies.getSdk();
          if (sdk && typeof sdk.host === 'string' && sdk.host.length > 0) {
            const protocol = dependencies.location.protocol === 'https:' ? 'https' : 'http';
            const nextHost = `${protocol}://${dependencies.location.host}/integrations/lockr/api`;
            const originalHost = sdk.host;
            sdk.host = nextHost;
            ownedSdk = sdk;
            previousHost = originalHost;
            installedHost = nextHost;
            return;
          }
        } catch {
          // Treat an unreadable or unwritable SDK as not ready.
        }
        if (attempts >= 50) {
          dependencies.timedOut();
          return;
        }
        try {
          timer = dependencies.setTimeout(check, 50);
        } catch {
          dependencies.timedOut();
        }
      };
      check();
    },
  });
}

export function createLockrIntegrationRegistration(release: string): IntegrationRegistration {
  return createLifecycleIntegrationRegistration(LOCKR_INTEGRATION_ID, release, {
    validateConfig: (candidate) => candidate === undefined,
  });
}
