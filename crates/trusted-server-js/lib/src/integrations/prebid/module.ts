import type { BrowserAuctionBidV1, BrowserAuctionProjectionV1 } from '../../core/types';
import {
  createBrowserPrebidAdapter,
  type PrebidAdapter,
  type PrebidEventFacade,
  type PrebidTrustedServerAuctionV1,
  type PreparedTrustedBidV1,
} from '../../adapters/prebid';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';
import {
  persistentFirstDisplaySliceSelectedV1,
  snapshotPersistentFirstDisplaySliceStateV1,
} from '../../shared/takeover';
import type { NavigationSession } from '../../kernel/sessions';
import type { RuntimeCapabilityV1 } from '../../kernel/runtime';
import { isPrebidIntegrationConfigV1 } from '../../shared/integration_config_validators';

import { createPrebidStartup } from './startup';

export const PREBID_INTEGRATION_ID = 'prebid' as const;
export type { PreparedTrustedBidV1 } from '../../adapters/prebid';

const arrayIsArrayIntrinsic = Array.isArray;
const objectGetOwnPropertyDescriptorIntrinsic = Object.getOwnPropertyDescriptor;
export interface PrebidCapabilityV1 {
  readonly adapter: PrebidAdapter;
  readonly clientSideBidders: readonly string[];
  readonly excludedGamAdUnitPathSuffixes: readonly string[];
}

interface ProductionPrebidSelectionCoordinator {
  readonly track: (
    preparedBid: Readonly<PreparedTrustedBidV1>,
    navigation: NavigationSession
  ) => boolean;
  readonly auctionEnded: (event: unknown, prebid: Readonly<PrebidEventFacade>) => void;
  readonly settlePublicationFailure: (
    navigation: NavigationSession,
    auctionId: string,
    adUnitCode: string,
    reason: 'prebid_admission_failed' | 'prebid_contract_violation'
  ) => boolean;
  readonly dispose: () => void;
}

interface ProductionPrebidPublicationInput {
  readonly admitTrustedBid: (preparedBid: Readonly<PreparedTrustedBidV1>) => unknown;
  readonly auctionId: string;
  readonly adUnitCode: string;
  readonly bid: BrowserAuctionBidV1;
  readonly generatedBid: unknown;
  readonly trackAdmittedBid: (
    preparedBid: Readonly<PreparedTrustedBidV1>,
    navigation: NavigationSession
  ) => boolean;
}

type ProductionPrebidPublicationResult =
  | Readonly<{ ok: true; bid: Readonly<PreparedTrustedBidV1> }>
  | Readonly<{
      ok: false;
      reason:
        | 'descriptor_invalid'
        | 'prebid_admission_failed'
        | 'prebid_contract_violation'
        | 'registry_full'
        | 'reservation_collision'
        | 'winner_not_renderable';
    }>;

interface ProductionRenderCapability {
  readonly createPrebidSelectionCoordinator: () => ProductionPrebidSelectionCoordinator;
  readonly navigation: NavigationSession;
  readonly publishPrebidBid: (
    input: ProductionPrebidPublicationInput
  ) => ProductionPrebidPublicationResult;
  readonly projection: Readonly<BrowserAuctionProjectionV1>;
}

function configStringArray(config: unknown, key: string): readonly string[] {
  if (typeof config !== 'object' || config === null) return Object.freeze([]);
  const descriptor = objectGetOwnPropertyDescriptorIntrinsic(config, key);
  if (!descriptor || !('value' in descriptor) || !arrayIsArrayIntrinsic(descriptor.value)) {
    return Object.freeze([]);
  }
  const accepted: string[] = [];
  for (const value of descriptor.value as readonly unknown[]) {
    if (typeof value === 'string') accepted.push(value);
  }
  return Object.freeze(accepted);
}

function exactCapability<Value extends object>(
  interfaces: Readonly<Record<string, unknown>>,
  key: string
): Value {
  const value = interfaces[key];
  if (typeof value !== 'object' || value === null || !Object.isFrozen(value)) {
    throw new TypeError(`Prebid requires ${key}`);
  }
  return value as Value;
}

/** Build the release-bound Prebid module registered by the coordinated runtime. */
export function createPrebidIntegrationRegistration(releaseId: string): IntegrationRegistration {
  const prepare = ({ config, interfaces, onDispose }: IntegrationPrepareContext) => {
    if (!isPrebidIntegrationConfigV1(config)) {
      throw new TypeError('Prebid integration config is invalid');
    }
    const runtime = exactCapability<RuntimeCapabilityV1>(interfaces, 'runtime.v1');
    const render = exactCapability<ProductionRenderCapability>(interfaces, 'render.v1');
    exactCapability(interfaces, 'slots.v1');
    exactCapability(interfaces, 'messages.v1');
    const runtimeWindow = runtime?.document?.defaultView;
    if (
      !runtimeWindow ||
      typeof render.navigation?.isCurrent !== 'function' ||
      !Object.isFrozen(render.projection) ||
      typeof render.createPrebidSelectionCoordinator !== 'function' ||
      typeof render.publishPrebidBid !== 'function'
    ) {
      throw new TypeError('Prebid capability graph is malformed');
    }
    const clientSideBidders = configStringArray(config, 'clientSideBidders');
    const excludedGamAdUnitPathSuffixes = configStringArray(
      config,
      'excludedGamAdUnitPathSuffixes'
    );
    const adapter = createBrowserPrebidAdapter(
      runtimeWindow as unknown as Parameters<typeof createBrowserPrebidAdapter>[0],
      { configuredClientSideBidders: clientSideBidders, requiredUserIdModules: Object.freeze([]) }
    );
    let active = false;
    let runtimeRelease: (() => void) | undefined;
    const coordinator = render.createPrebidSelectionCoordinator();
    const completeAuction = (auctionRequest: Readonly<PrebidTrustedServerAuctionV1>): void => {
      try {
        auctionRequest.complete();
      } catch {
        // Publisher completion remains isolated from the owned auction transaction.
      }
    };
    const publishAuction = (auctionRequest: Readonly<PrebidTrustedServerAuctionV1>): void => {
      const navigation = render.navigation;
      if (!active || !navigation.isCurrent()) {
        completeAuction(auctionRequest);
        return;
      }
      try {
        const projection = navigation.currentAuctionProjection as
          Readonly<BrowserAuctionProjectionV1> | undefined;
        if (!projection || projection !== render.projection) return;
        if (projection.auction.auctionId !== auctionRequest.auctionId) return;
        for (let index = 0; index < auctionRequest.bids.length; index += 1) {
          const request = auctionRequest.bids[index];
          if (!request) continue;
          const winners = projection.auction.results.filter(
            (result) => result.slot === request.adUnitCode && result.outcome === 'winner'
          );
          if (winners.length !== 1) continue;
          const winner = winners[0];
          if (!winner || winner.outcome !== 'winner') continue;
          const bids = projection.bids.filter(
            (bid) => bid.slot === request.adUnitCode && bid.candidateId === winner.candidateId
          );
          if (bids.length !== 1) continue;
          const bid = bids[0];
          if (!bid) continue;
          const publication = render.publishPrebidBid({
            admitTrustedBid: adapter.admitTrustedBid,
            auctionId: auctionRequest.auctionId,
            adUnitCode: request.adUnitCode,
            bid,
            generatedBid: Object.freeze({
              requestId: request.requestId,
              adId: request.requestId,
              cpm: bid.cpm,
              width: bid.renderSource.width,
              height: bid.renderSource.height,
            }),
            trackAdmittedBid: coordinator.track,
          });
          if (
            !publication.ok &&
            (publication.reason === 'prebid_admission_failed' ||
              publication.reason === 'prebid_contract_violation')
          ) {
            coordinator.settlePublicationFailure(
              navigation,
              auctionRequest.auctionId,
              request.adUnitCode,
              publication.reason
            );
          }
        }
      } catch {
        // Invalid or stale projection state publishes no TS bid.
      } finally {
        completeAuction(auctionRequest);
      }
    };
    const startup = createPrebidStartup({
      dispose: () => undefined,
      onAuction: publishAuction,
      onAuctionEnd: coordinator.auctionEnded,
      prebid: adapter,
    });
    const dispose = (): void => {
      active = false;
      const release = runtimeRelease;
      runtimeRelease = undefined;
      release?.();
      coordinator.dispose();
      adapter.dispose();
    };
    onDispose(dispose);
    const capability: PrebidCapabilityV1 = Object.freeze({
      adapter,
      clientSideBidders,
      excludedGamAdUnitPathSuffixes,
    });

    return Object.freeze({
      activate: ({ adoption, afterCommit, onDispose }: IntegrationActivationContext) => {
        if (active) throw new Error('Prebid integration is already active');
        if (adoption !== undefined) {
          const selected = persistentFirstDisplaySliceSelectedV1(adoption, 'prebid_initial');
          const initialState = selected
            ? snapshotPersistentFirstDisplaySliceStateV1(adoption, 'prebid_initial')
            : undefined;
          if (
            selected === undefined ||
            (selected &&
              (!initialState ||
                initialState.values.length !== 1 ||
                initialState.values[0]?.[0] !== 'protocol_version' ||
                initialState.values[0][1] !== 1))
          ) {
            throw new TypeError('Prebid first-display adoption is invalid');
          }
        }
        active = true;
        onDispose(dispose);
        afterCommit(() => {
          if (!active) return;
          runtimeRelease = startup.activate();
          startup.start(config);
        });
      },
      interfaces: Object.freeze({ 'prebid.v1': capability }),
    });
  };
  return Object.freeze({
    abi: 1,
    id: PREBID_INTEGRATION_ID,
    phase: 'takeover',
    releaseId,
    prepareSync: prepare,
    prepare,
  });
}

export type {
  PrebidRefreshAuctionOperation,
  PrebidRefreshAuctionPreparation,
  PrebidRefreshPolicy,
  PrebidRefreshPolicyOptions,
  PrebidRegisteredRefreshAuctionOptions,
  PrebidSyntheticRefreshRunner,
  PrebidSyntheticRefreshRunnerOptions,
} from './refresh';
