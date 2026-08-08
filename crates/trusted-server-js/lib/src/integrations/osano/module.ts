import type { IntegrationRegistration } from '../../kernel/integration_registry';
import {
  createLifecycleIntegrationRegistration,
  type IntegrationLifecycleRuntime,
} from '../../kernel/lifecycle_module';

import { disposeOsanoConsentMirror, initializeOsanoConsentMirror } from './consent_mirror';

export const OSANO_INTEGRATION_ID = 'osano' as const;

export interface OsanoRuntimeDependencies {
  readonly initialize: () => void;
  readonly reset: () => void;
}

/** Bind the existing consent mirror's complete lifecycle to one release. */
export function createOsanoRuntime(
  dependencies: OsanoRuntimeDependencies = {
    initialize: initializeOsanoConsentMirror,
    reset: disposeOsanoConsentMirror,
  }
): IntegrationLifecycleRuntime {
  let active = false;
  let started = false;
  return Object.freeze({
    activate: (_config: unknown) => {
      if (active) throw new Error('Osano runtime is already active');
      active = true;
      started = false;
      return (): void => {
        if (!active) return;
        active = false;
        started = false;
        dependencies.reset();
      };
    },
    start: (_config: unknown) => {
      if (!active || started) return;
      started = true;
      dependencies.initialize();
    },
  });
}

export function createOsanoIntegrationRegistration(release: string): IntegrationRegistration {
  return createLifecycleIntegrationRegistration(OSANO_INTEGRATION_ID, release, {
    validateConfig: (candidate) => candidate === undefined,
  });
}
