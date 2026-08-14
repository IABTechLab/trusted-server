import type { IntegrationRegistration } from '../../kernel/integration_registry';
import {
  createLifecycleIntegrationRegistration,
  type IntegrationLifecycleRuntime,
} from '../../kernel/lifecycle_module';
import { log } from '../../core/log';

import {
  installGtmBeaconGuard,
  installGtmGuard,
  resetBeaconGuardState,
  resetGuardState,
} from './script_guard';

export const GOOGLE_TAG_MANAGER_INTEGRATION_ID = 'google_tag_manager' as const;

export interface GoogleTagManagerRuntimeDependencies {
  readonly installBeaconGuard: () => void;
  readonly installScriptGuard: () => void;
  readonly resetBeaconGuard: () => void;
  readonly resetScriptGuard: () => void;
  readonly started: () => void;
}

/** Own the reversible GTM script/preload and GA network guards for one runtime. */
export function createGoogleTagManagerRuntime(
  dependencies: GoogleTagManagerRuntimeDependencies = {
    installBeaconGuard: installGtmBeaconGuard,
    installScriptGuard: installGtmGuard,
    resetBeaconGuard: resetBeaconGuardState,
    resetScriptGuard: resetGuardState,
    started: () => log.info('Google Tag Manager integration initialized'),
  }
): IntegrationLifecycleRuntime {
  return Object.freeze({
    activate: (_config: unknown) => {
      let beaconAttempted = false;
      try {
        dependencies.installScriptGuard();
        beaconAttempted = true;
        dependencies.installBeaconGuard();
      } catch (error) {
        if (beaconAttempted) {
          try {
            dependencies.resetBeaconGuard();
          } catch {
            // Continue through independent script-guard rollback.
          }
        }
        try {
          dependencies.resetScriptGuard();
        } catch {
          // Preserve the activation failure after best-effort rollback.
        }
        throw error;
      }
      let active = true;
      return (): void => {
        if (!active) return;
        active = false;
        try {
          dependencies.resetBeaconGuard();
        } finally {
          dependencies.resetScriptGuard();
        }
      };
    },
    start: (_config: unknown) => dependencies.started(),
  });
}

export function createGoogleTagManagerIntegrationRegistration(
  release: string
): IntegrationRegistration {
  return createLifecycleIntegrationRegistration(GOOGLE_TAG_MANAGER_INTEGRATION_ID, release, {
    createOwnedRuntime: () => createGoogleTagManagerRuntime(),
    firstDisplaySliceId: 'google_tag_manager_initial',
    validateConfig: (candidate) => candidate === undefined,
    validateFirstDisplayState: (state) =>
      state.values.length === 1 &&
      state.values[0]?.[0] === 'route_guard' &&
      state.values[0][1] === 'google_tag_manager',
  });
}
