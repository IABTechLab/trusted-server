import { EMBEDDED_RELEASE_ID } from '../../core/release';
import { isSourcepointIntegrationConfigV1 } from '../../shared/integration_config_validators';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';

interface SourcepointConsentCapabilityV1 {
  readonly activateLifecycle: () => () => void;
  readonly startLifecycle: () => void;
}

/** Build the release-bound later Sourcepoint lifecycle registration. */
export function createSourcepointLifecycleIntegrationRegistration(
  releaseId: string
): IntegrationRegistration {
  return Object.freeze({
    abi: 1,
    id: 'sourcepoint_lifecycle',
    phase: 'deferred',
    releaseId,
    prepare: ({ config, interfaces }: IntegrationPrepareContext) => {
      const capability = interfaces['sourcepoint_consent.v1'] as
        SourcepointConsentCapabilityV1 | undefined;
      if (
        !isSourcepointIntegrationConfigV1(config) ||
        !capability ||
        !Object.isFrozen(capability) ||
        typeof capability.activateLifecycle !== 'function' ||
        typeof capability.startLifecycle !== 'function'
      ) {
        throw new TypeError('Sourcepoint lifecycle capability graph is invalid');
      }
      return Object.freeze({
        activate: ({ afterCommit, onDispose }: IntegrationActivationContext) => {
          const ownership: { release?: () => void } = {};
          onDispose(() => ownership.release?.());
          ownership.release = capability.activateLifecycle();
          afterCommit(capability.startLifecycle);
        },
      });
    },
  });
}

if (typeof window !== 'undefined') {
  const register = (window.tsjs as unknown as { _registerIntegration?: unknown } | undefined)
    ?._registerIntegration;
  if (typeof register === 'function') {
    Reflect.apply(register, window.tsjs, [
      createSourcepointLifecycleIntegrationRegistration(EMBEDDED_RELEASE_ID),
    ]);
  }
}
