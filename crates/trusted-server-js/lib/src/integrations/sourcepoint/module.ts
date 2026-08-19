import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';
import type { IntegrationLifecycleRuntime } from '../../kernel/lifecycle_module';
import { isSourcepointIntegrationConfigV1 } from '../../shared/integration_config_validators';
import { validatePersistentFirstDisplaySliceAdoptionV1 } from '../../shared/takeover';

import {
  disposeSourcepointConsentMirror,
  initializeSourcepointConsentMirror,
  mirrorSourcepointConsent,
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

export interface SourcepointConsentCapabilityV1 {
  readonly activateLifecycle: () => () => void;
  readonly startLifecycle: () => void;
}

const sourcepointBootConfig = (candidate: unknown): candidate is SourcepointBootConfig =>
  isSourcepointIntegrationConfigV1(candidate);

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
  return Object.freeze({
    abi: 1,
    id: SOURCEPOINT_INTEGRATION_ID,
    phase: 'takeover',
    releaseId: release,
    prepare: ({ config, interfaces, onDispose }: IntegrationPrepareContext) => {
      const runtimeCapability = interfaces['runtime.v1'];
      if (
        !sourcepointBootConfig(config) ||
        typeof runtimeCapability !== 'object' ||
        runtimeCapability === null ||
        !Object.isFrozen(runtimeCapability)
      ) {
        throw new TypeError('Sourcepoint consent capability graph is invalid');
      }
      const lifecycle = createSourcepointRuntime({
        initializeConsentMirror: initializeSourcepointConsentMirror,
        installGuard: () => undefined,
        resetConsentMirror: disposeSourcepointConsentMirror,
        resetGuard: () => undefined,
      });
      let takeoverActive = false;
      let guardInstalled = false;
      let lifecycleRelease: (() => void) | undefined;
      const capability: SourcepointConsentCapabilityV1 = Object.freeze({
        activateLifecycle: () => {
          if (!takeoverActive || lifecycleRelease) {
            throw new TypeError('Sourcepoint lifecycle is unavailable');
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
            throw new TypeError('Sourcepoint lifecycle is not active');
          }
          lifecycle.start(config);
        },
      });
      const releaseOwnedState = (): void => {
        takeoverActive = false;
        const releaseLifecycle = lifecycleRelease;
        lifecycleRelease = undefined;
        releaseLifecycle?.();
        disposeSourcepointConsentMirror();
        if (guardInstalled) {
          guardInstalled = false;
          resetGuardState();
        }
      };
      onDispose(releaseOwnedState);
      return Object.freeze({
        activate: ({
          adoption,
          afterCommit,
          onDispose: onActivationDispose,
        }: IntegrationActivationContext) => {
          if (takeoverActive) throw new Error('Sourcepoint consent is already active');
          if (
            adoption !== undefined &&
            !validatePersistentFirstDisplaySliceAdoptionV1(
              adoption,
              'sourcepoint_initial',
              (state) => {
                const value = state.values[0]?.[1];
                return (
                  state.values.length === 1 &&
                  state.values[0]?.[0] === 'gpp_snapshot' &&
                  typeof value === 'number' &&
                  Number.isInteger(value) &&
                  value >= 0
                );
              }
            )
          ) {
            throw new TypeError('Sourcepoint first-display parser state is invalid');
          }
          if (config.rewriteSdk) {
            installSourcepointGuard();
            guardInstalled = true;
          }
          takeoverActive = true;
          onActivationDispose(releaseOwnedState);
          afterCommit(() => {
            mirrorSourcepointConsent();
          });
        },
        interfaces: Object.freeze({ 'sourcepoint_consent.v1': capability }),
      });
    },
  });
}
