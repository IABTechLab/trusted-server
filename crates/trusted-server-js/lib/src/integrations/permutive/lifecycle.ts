import { EMBEDDED_RELEASE_ID } from '../../core/release';
import { isEmptyIntegrationConfigV1 } from '../../shared/integration_config_validators';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';

interface PermutiveContextCapabilityV1 {
  readonly activateLifecycle: () => () => void;
  readonly startLifecycle: () => void;
}

/** Build the release-bound later Permutive lifecycle registration. */
export function createPermutiveLifecycleIntegrationRegistration(
  releaseId: string
): IntegrationRegistration {
  return Object.freeze({
    abi: 1,
    id: 'permutive_lifecycle',
    phase: 'deferred',
    releaseId,
    prepare: ({ config, interfaces }: IntegrationPrepareContext) => {
      const capability = interfaces['permutive_context.v1'] as
        PermutiveContextCapabilityV1 | undefined;
      if (
        !isEmptyIntegrationConfigV1(config) ||
        !capability ||
        !Object.isFrozen(capability) ||
        typeof capability.activateLifecycle !== 'function' ||
        typeof capability.startLifecycle !== 'function'
      ) {
        throw new TypeError('Permutive lifecycle capability graph is invalid');
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
      createPermutiveLifecycleIntegrationRegistration(EMBEDDED_RELEASE_ID),
    ]);
  }
}
