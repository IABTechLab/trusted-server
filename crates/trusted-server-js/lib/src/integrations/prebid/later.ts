import { EMBEDDED_RELEASE_ID } from '../../core/release';
import { isPrebidIntegrationConfigV1 } from '../../shared/integration_config_validators';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';
import type { NavigationSession } from '../../kernel/sessions';

import {
  createPrebidRefreshPolicy,
  createPrebidSyntheticRefreshRunner,
  preparePrebidRegisteredRefreshAuction,
  type DeferredGoogletagRunner,
  type DeferredPrebidRunner,
  type PrebidRefreshPolicy,
} from './refresh';

interface GptLaterCapabilityV1 {
  readonly adapter: DeferredGoogletagRunner;
  readonly directAuctionUnitForSlot: (slot: object) => Readonly<object> | undefined;
  readonly installRefreshPolicy: (policy: PrebidRefreshPolicy) => (() => void) | undefined;
  readonly navigation: () => NavigationSession | undefined;
}

interface PrebidLaterCapabilityV1 {
  readonly adapter: DeferredPrebidRunner;
  readonly clientSideBidders: readonly string[];
  readonly excludedGamAdUnitPathSuffixes: readonly string[];
}

/** Build the release-bound synthetic-refresh Prebid registration. */
export function createPrebidLaterIntegrationRegistration(
  releaseId: string
): IntegrationRegistration {
  return Object.freeze({
    abi: 1,
    id: 'prebid_later',
    phase: 'deferred',
    releaseId,
    prepare: ({ config, interfaces }: IntegrationPrepareContext) => {
      const runtime = interfaces['runtime.v1'];
      const slots = interfaces['slots.v1'];
      const gpt = interfaces['gpt.v1'] as GptLaterCapabilityV1 | undefined;
      const prebid = interfaces['prebid.v1'] as PrebidLaterCapabilityV1 | undefined;
      if (
        !isPrebidIntegrationConfigV1(config) ||
        typeof runtime !== 'object' ||
        runtime === null ||
        !Object.isFrozen(runtime) ||
        typeof slots !== 'object' ||
        slots === null ||
        !Object.isFrozen(slots) ||
        !gpt ||
        !Object.isFrozen(gpt) ||
        typeof gpt.installRefreshPolicy !== 'function' ||
        typeof gpt.navigation !== 'function' ||
        typeof gpt.directAuctionUnitForSlot !== 'function' ||
        !prebid ||
        !Object.isFrozen(prebid)
      ) {
        throw new TypeError('Prebid later capability graph is invalid');
      }
      const runner = createPrebidSyntheticRefreshRunner({
        prebid: prebid.adapter,
        prepareAuction: (physicalSlots) =>
          preparePrebidRegisteredRefreshAuction({
            clientSideBidders: prebid.clientSideBidders,
            resolveAdUnit: gpt.directAuctionUnitForSlot,
            slots: physicalSlots,
          }),
      });
      const policy = createPrebidRefreshPolicy({
        currentNavigation: gpt.navigation,
        excludedGamAdUnitPathSuffixes: prebid.excludedGamAdUnitPathSuffixes,
        googletag: gpt.adapter,
        runSyntheticAuction: runner,
      });
      return Object.freeze({
        activate: ({ onDispose }: IntegrationActivationContext) => {
          const ownership: { release?: () => void } = {};
          onDispose(() => {
            ownership.release?.();
            policy.dispose();
          });
          const release = gpt.installRefreshPolicy(policy);
          if (!release) throw new TypeError('Prebid refresh policy is duplicated');
          ownership.release = release;
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
      createPrebidLaterIntegrationRegistration(EMBEDDED_RELEASE_ID),
    ]);
  }
}
