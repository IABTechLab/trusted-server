import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';
import type { IntegrationLifecycleRuntime } from '../../kernel/lifecycle_module';
import { isEmptyIntegrationConfigV1 } from '../../shared/integration_config_validators';
import { validatePersistentFirstDisplaySliceAdoptionV1 } from '../../shared/takeover';

import {
  disposeOsanoConsentMirror,
  initializeOsanoConsentMirror,
  mirrorOsanoConsent,
} from './consent_mirror';

export const OSANO_INTEGRATION_ID = 'osano_consent' as const;

export interface OsanoRuntimeDependencies {
  readonly initialize: () => void;
  readonly reset: () => void;
}

export interface OsanoConsentCapabilityV1 {
  readonly activateLifecycle: () => () => void;
  readonly startLifecycle: () => void;
}

function runtimeCapability(interfaces: Readonly<Record<string, unknown>>): boolean {
  const runtime = interfaces['runtime.v1'];
  return typeof runtime === 'object' && runtime !== null && Object.isFrozen(runtime);
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
  return Object.freeze({
    abi: 1,
    id: OSANO_INTEGRATION_ID,
    phase: 'takeover',
    releaseId: release,
    prepare: ({ config, interfaces, onDispose }: IntegrationPrepareContext) => {
      if (!isEmptyIntegrationConfigV1(config) || !runtimeCapability(interfaces)) {
        throw new TypeError('Osano takeover capability graph is invalid');
      }
      const lifecycle = createOsanoRuntime();
      let takeoverActive = false;
      let lifecycleRelease: (() => void) | undefined;
      const capability: OsanoConsentCapabilityV1 = Object.freeze({
        activateLifecycle: () => {
          if (!takeoverActive || lifecycleRelease) {
            throw new TypeError('Osano lifecycle is unavailable');
          }
          lifecycleRelease = lifecycle.activate(config);
          return (): void => {
            const releaseLifecycle = lifecycleRelease;
            lifecycleRelease = undefined;
            releaseLifecycle?.();
          };
        },
        startLifecycle: () => {
          if (!takeoverActive || !lifecycleRelease) {
            throw new TypeError('Osano lifecycle is not active');
          }
          lifecycle.start(config);
        },
      });
      onDispose(() => {
        takeoverActive = false;
        const releaseLifecycle = lifecycleRelease;
        lifecycleRelease = undefined;
        releaseLifecycle?.();
        disposeOsanoConsentMirror();
      });
      return Object.freeze({
        activate: ({
          adoption,
          afterCommit,
          onDispose: onActivationDispose,
        }: IntegrationActivationContext) => {
          if (takeoverActive) throw new Error('Osano takeover slice is already active');
          if (
            adoption !== undefined &&
            !validatePersistentFirstDisplaySliceAdoptionV1(
              adoption,
              'osano_initial',
              (state) =>
                state.values.length === 0 ||
                (state.values.length === 1 &&
                  state.values[0]?.[0] === 'consent_snapshot' &&
                  typeof state.values[0][1] === 'number' &&
                  Number.isInteger(state.values[0][1]) &&
                  state.values[0][1] >= 0)
            )
          ) {
            throw new TypeError('Osano first-display parser state is invalid');
          }
          takeoverActive = true;
          onActivationDispose(() => {
            takeoverActive = false;
            disposeOsanoConsentMirror();
          });
          afterCommit(() => {
            void mirrorOsanoConsent();
          });
        },
        interfaces: Object.freeze({ 'osano_consent.v1': capability }),
      });
    },
  });
}
