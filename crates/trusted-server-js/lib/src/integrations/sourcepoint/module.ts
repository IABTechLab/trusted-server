import type { IntegrationRegistration } from '../../kernel/integration_registry';
import {
  createLifecycleIntegrationRegistration,
  type IntegrationLifecycleRuntime,
} from '../../kernel/lifecycle_module';

import {
  disposeSourcepointConsentMirror,
  initializeSourcepointConsentMirror,
} from './consent_mirror';
import { installSourcepointGuard, resetGuardState } from './script_guard';

export const SOURCEPOINT_INTEGRATION_ID = 'sourcepoint_consent' as const;

interface SourcepointBootConfig {
  readonly rewriteSdk: boolean;
}

export interface SourcepointRuntimeDependencies {
  readonly initializeConsentMirror: () => void;
  readonly installGuard: () => void;
  readonly resetConsentMirror: () => void;
  readonly resetGuard: () => void;
}

function sourcepointBootConfig(candidate: unknown): candidate is SourcepointBootConfig {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      !Object.isFrozen(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      Reflect.ownKeys(candidate).length !== 1
    ) {
      return false;
    }
    const rewriteSdk = Object.getOwnPropertyDescriptor(candidate, 'rewriteSdk');
    return Boolean(
      rewriteSdk?.enumerable && 'value' in rewriteSdk && typeof rewriteSdk.value === 'boolean'
    );
  } catch {
    return false;
  }
}

/** Own Sourcepoint's optional guard and consent mirror as one release-bound unit. */
export function createSourcepointRuntime(
  dependencies: SourcepointRuntimeDependencies = {
    initializeConsentMirror: initializeSourcepointConsentMirror,
    installGuard: installSourcepointGuard,
    resetConsentMirror: disposeSourcepointConsentMirror,
    resetGuard: resetGuardState,
  }
): IntegrationLifecycleRuntime {
  let active = false;
  let guardInstalled = false;
  let started = false;
  return Object.freeze({
    activate: (candidate: unknown) => {
      if (!sourcepointBootConfig(candidate)) {
        throw new TypeError('Sourcepoint integration config is invalid');
      }
      if (active) throw new Error('Sourcepoint runtime is already active');
      if (candidate.rewriteSdk) {
        try {
          dependencies.installGuard();
          guardInstalled = true;
        } catch (error) {
          try {
            dependencies.resetGuard();
          } catch {
            // Preserve the activation failure after best-effort rollback.
          }
          throw error;
        }
      }
      active = true;
      started = false;
      return (): void => {
        if (!active) return;
        active = false;
        started = false;
        try {
          dependencies.resetConsentMirror();
        } finally {
          if (guardInstalled) {
            guardInstalled = false;
            dependencies.resetGuard();
          }
        }
      };
    },
    start: (_config: unknown) => {
      if (!active || started) return;
      started = true;
      dependencies.initializeConsentMirror();
    },
  });
}

export function createSourcepointIntegrationRegistration(release: string): IntegrationRegistration {
  return createLifecycleIntegrationRegistration(SOURCEPOINT_INTEGRATION_ID, release, {
    validateConfig: sourcepointBootConfig,
  });
}
