import type { IntegrationRegistration } from '../../kernel/integration_registry';
import { isEmptyIntegrationConfigV1 } from '../../shared/integration_config_validators';
import {
  createLifecycleIntegrationRegistration,
  type IntegrationLifecycleRuntime,
} from '../../kernel/lifecycle_module';
import { log } from '../../core/log';

import { installDataDomeGuard, resetGuardState } from './script_guard';

export const DATADOME_INTEGRATION_ID = 'datadome' as const;

export interface DataDomeRuntimeDependencies {
  readonly installGuard: () => void;
  readonly resetGuard: () => void;
  readonly started: () => void;
}

/** Own the reversible DataDome script/preload guard for one runtime. */
export function createDataDomeRuntime(
  dependencies: DataDomeRuntimeDependencies = {
    installGuard: installDataDomeGuard,
    resetGuard: resetGuardState,
    started: () => log.info('DataDome integration initialized'),
  }
): IntegrationLifecycleRuntime {
  return Object.freeze({
    activate: (_config: unknown) => {
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
      let active = true;
      return (): void => {
        if (!active) return;
        active = false;
        dependencies.resetGuard();
      };
    },
    start: (_config: unknown) => dependencies.started(),
  });
}

export function createDataDomeIntegrationRegistration(release: string): IntegrationRegistration {
  return createLifecycleIntegrationRegistration(DATADOME_INTEGRATION_ID, release, {
    createOwnedRuntime: () => createDataDomeRuntime(),
    firstDisplaySliceId: 'datadome_initial',
    validateConfig: isEmptyIntegrationConfigV1,
    validateFirstDisplayState: (state) =>
      state.values.length === 1 &&
      state.values[0]?.[0] === 'route_guard' &&
      state.values[0][1] === 'datadome',
  });
}
