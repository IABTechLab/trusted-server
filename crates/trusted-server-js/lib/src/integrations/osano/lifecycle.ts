import { EMBEDDED_RELEASE_ID } from '../../core/release';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';

interface OsanoConsentCapabilityV1 {
  readonly activateLifecycle: () => () => void;
  readonly startLifecycle: () => void;
}

/** Build the release-bound later Osano lifecycle registration. */
export function createOsanoLifecycleIntegrationRegistration(
  releaseId: string
): IntegrationRegistration {
  return Object.freeze({
    abi: 1,
    id: 'osano_lifecycle',
    phase: 'deferred',
    releaseId,
    prepare: ({ config, interfaces }: IntegrationPrepareContext) => {
      const capability = interfaces['osano_consent.v1'] as OsanoConsentCapabilityV1 | undefined;
      if (
        config !== undefined ||
        !capability ||
        !Object.isFrozen(capability) ||
        typeof capability.activateLifecycle !== 'function' ||
        typeof capability.startLifecycle !== 'function'
      ) {
        throw new TypeError('Osano lifecycle capability graph is invalid');
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
      createOsanoLifecycleIntegrationRegistration(EMBEDDED_RELEASE_ID),
    ]);
  }
}
