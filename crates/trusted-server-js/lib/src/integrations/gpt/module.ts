import {
  persistentFirstDisplaySliceSelectedV1,
  snapshotPersistentFirstDisplayAdoptionV1,
  snapshotPersistentFirstDisplaySliceStateV1,
  type FirstDisplayGptDiagnosticEventV1,
  type FirstDisplayGptDiagnosticsV1,
  type PersistentFirstDisplayAdoptionV1,
} from '../../shared/takeover';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
  PreparedIntegration,
} from '../../kernel/integration_registry';
import {
  createBrowserGoogletagAdapter,
  type GoogletagAdapter,
  type GoogletagFacade,
} from '../../adapters/googletag';
import type {
  BaselinePbsCacheSourceV1,
  BrowserAuctionBidV1,
  BrowserAuctionProjectionV1,
  BrowserAuctionSlotV1,
} from '../../core/types';
import { isGptIntegrationConfigV1 } from '../../shared/integration_config_validators';
import { log } from '../../core/log';
import { DisposableStack } from '../../kernel/disposable';
import type { RuntimeCapabilityV1 } from '../../kernel/runtime';
import type { NavigationSession, RenderAttemptScope, RuntimeSession } from '../../kernel/sessions';
import type { IdentityGenerationResult } from '../../kernel/identity';
import type {
  CommittedArtifactStore,
  CommittedRenderArtifact,
  RenderAttempt,
  RenderAttemptCreationResult,
  RenderFailureReason,
  SlotOperationCreationResult,
  SlotOperationOptions,
} from '../../services/render';
import { resizeCollapsedPucShell } from '../../core/puc_shell';
import {
  createPucBridge,
  type PucBridge,
  type PucBridgeOptions,
  type PucGamAttemptInput,
} from '../../services/puc_bridge';
import type { ReservationService } from '../../services/reservations';
import {
  createBrowserSlotReconciliationBoundary,
  createSlotService,
  type GptSlotBinding,
  type SlotRequestOutcome,
  type SlotService,
} from '../../services/slots';
import type {
  TargetingBoundary,
  TargetingOwnership,
  TargetingService,
} from '../../services/targeting';
import { createTargetingService } from '../../services/targeting';

import {
  activateGptDiagnosticsEventListeners,
  createGptDiagnosticsFactBuffer,
  projectGptTraceFact,
} from './diagnostics_facts';
import { installGptGuard, resetGuardState } from './script_guard';
import { createGptStartup } from './startup';

export const GPT_INTEGRATION_ID = 'gpt' as const;

function frozenArray(candidate: unknown): readonly unknown[] | undefined {
  return Array.isArray(candidate) && Object.isFrozen(candidate) ? candidate : undefined;
}

function dataField(candidate: unknown, key: string): unknown {
  if (typeof candidate !== 'object' || candidate === null || Array.isArray(candidate)) {
    return undefined;
  }
  const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
  return descriptor?.enumerable && 'value' in descriptor ? descriptor.value : undefined;
}

/** Hydrate exact GPT diagnostic tokens/cycles before adopted slots become observable. */
export function adoptInitialGptDiagnosticsFromHandoff(
  candidate: unknown,
  adapter: Pick<GoogletagAdapter, 'adoptDiagnosticsState'>
): PersistentFirstDisplayAdoptionV1 | undefined {
  const adoption = snapshotPersistentFirstDisplayAdoptionV1(candidate);
  const adopt = adapter.adoptDiagnosticsState;
  if (!adoption || typeof adopt !== 'function') return undefined;
  const cycles = frozenArray(dataField(adoption.handoff, 'cycles'));
  const artifacts = frozenArray(dataField(adoption.handoff, 'artifacts'));
  const trace = dataField(adoption.handoff, 'trace');
  const nextTraceTokenOrdinal = dataField(trace, 'nextGlobalSlotOrdinal');
  if (
    !cycles ||
    !artifacts ||
    typeof nextTraceTokenOrdinal !== 'number' ||
    adoption.identities.length !== cycles.length + artifacts.length
  ) {
    return undefined;
  }
  const slots: Array<{
    nextCycleOrdinal: number;
    physicalSlot: object;
    records: readonly Readonly<{
      ordinal: number;
      responseIdentifier: string | null;
      seen: readonly FirstDisplayGptDiagnosticEventV1[];
      state: 'open' | 'completed' | 'retired';
    }>[];
    traceToken: string;
    unknownPriorCycle: boolean;
  }> = [];
  for (let index = 0; index < cycles.length; index += 1) {
    const cycle = cycles[index];
    const physicalSlot = adoption.identities[index];
    const traceToken = dataField(cycle, 'token');
    const nextCycleOrdinal = dataField(cycle, 'nextCycleOrdinal');
    const unknownPriorCycle = dataField(cycle, 'unknownPriorCycle');
    const records = frozenArray(dataField(cycle, 'records'));
    if (
      !physicalSlot ||
      typeof traceToken !== 'string' ||
      typeof nextCycleOrdinal !== 'number' ||
      typeof unknownPriorCycle !== 'boolean' ||
      !records
    ) {
      return undefined;
    }
    const copiedRecords: Array<{
      ordinal: number;
      responseIdentifier: string | null;
      seen: readonly FirstDisplayGptDiagnosticEventV1[];
      state: 'open' | 'completed' | 'retired';
    }> = [];
    for (const record of records) {
      const ordinal = dataField(record, 'ordinal');
      const responseIdentifier = dataField(record, 'responseIdentifier');
      const seen = frozenArray(dataField(record, 'seen'));
      const state = dataField(record, 'state');
      if (
        typeof ordinal !== 'number' ||
        (responseIdentifier !== null && typeof responseIdentifier !== 'string') ||
        !seen ||
        seen.some(
          (event) =>
            event !== 'slotRequested' &&
            event !== 'slotResponseReceived' &&
            event !== 'slotRenderEnded' &&
            event !== 'slotOnload' &&
            event !== 'impressionViewable' &&
            event !== 'slotVisibilityChanged'
        ) ||
        (state !== 'open' && state !== 'completed' && state !== 'retired')
      ) {
        return undefined;
      }
      copiedRecords.push({
        ordinal,
        responseIdentifier,
        seen: seen as readonly FirstDisplayGptDiagnosticEventV1[],
        state,
      });
    }
    slots.push({
      nextCycleOrdinal,
      physicalSlot,
      records: copiedRecords,
      traceToken,
      unknownPriorCycle,
    });
  }
  try {
    return Reflect.apply(adopt, adapter, [{ nextTraceTokenOrdinal, slots }]) === true
      ? adoption
      : undefined;
  } catch {
    return undefined;
  }
}

/** Transfer unexpired lifecycle-ticket tombstones into the sole persistent PUC owner. */
export function adoptInitialPucTicketsFromHandoff(
  candidate: unknown,
  bridge: Pick<PucBridge, 'adoptFirstDisplayTickets'>
): PersistentFirstDisplayAdoptionV1 | undefined {
  const adoption = snapshotPersistentFirstDisplayAdoptionV1(candidate);
  if (!adoption) return undefined;
  const highWater = dataField(adoption.handoff, 'highWater');
  const clockEpochMs = dataField(highWater, 'reservationClockEpochMs');
  const nextTicketOrdinal = dataField(highWater, 'nextTicketOrdinal');
  const tombstones = frozenArray(dataField(adoption.handoff, 'tombstones'));
  if (typeof clockEpochMs !== 'number' || typeof nextTicketOrdinal !== 'number' || !tombstones) {
    return undefined;
  }
  const tickets: Array<{ expiresAtMs: number; ticket: string }> = [];
  for (const tombstone of tombstones) {
    if (dataField(tombstone, 'kind') !== 'ticket') continue;
    const ticket = dataField(tombstone, 'value');
    const expiresAtMs = dataField(tombstone, 'expiresAtMs');
    if (typeof ticket !== 'string' || typeof expiresAtMs !== 'number') return undefined;
    tickets.push({ expiresAtMs, ticket });
  }
  try {
    return bridge.adoptFirstDisplayTickets({
      clockEpochMs,
      nextTicketOrdinal,
      tombstones: tickets,
    })
      ? adoption
      : undefined;
  } catch {
    return undefined;
  }
}

/** Restore the ordered bounded diagnostics buffer without replaying facts as new events. */
export function adoptInitialGptFactsFromHandoff(
  candidate: unknown,
  buffer: Pick<ReturnType<typeof createGptDiagnosticsFactBuffer>, 'adoptFirstDisplay'> | undefined,
  adapter: Pick<GoogletagAdapter, 'diagnosticsIdentity'>
): PersistentFirstDisplayAdoptionV1 | undefined {
  const adoption = snapshotPersistentFirstDisplayAdoptionV1(candidate);
  if (!adoption) return undefined;
  const diagnostics = dataField(adoption.handoff, 'gptDiagnostics');
  const facts = frozenArray(dataField(diagnostics, 'facts'));
  const overflow = dataField(diagnostics, 'overflowCount');
  const drops = dataField(diagnostics, 'dropCount');
  if (!facts || typeof overflow !== 'number' || typeof drops !== 'number') return undefined;
  if (!buffer) {
    return facts.length === 0 && overflow === 0 && drops === 0 ? adoption : undefined;
  }
  const cycles = frozenArray(dataField(adoption.handoff, 'cycles'));
  const artifacts = frozenArray(dataField(adoption.handoff, 'artifacts'));
  if (!cycles || !artifacts || adoption.identities.length !== cycles.length + artifacts.length) {
    return undefined;
  }
  const identities = new Map<string, ReturnType<GoogletagAdapter['diagnosticsIdentity']>>();
  for (let index = 0; index < cycles.length; index += 1) {
    const token = dataField(cycles[index], 'token');
    const physicalSlot = adoption.identities[index];
    if (typeof token !== 'string' || !physicalSlot) return undefined;
    let identity: ReturnType<GoogletagAdapter['diagnosticsIdentity']>;
    try {
      identity = adapter.diagnosticsIdentity(physicalSlot);
    } catch {
      return undefined;
    }
    if (!identity || identity.traceToken !== token || identities.has(token)) return undefined;
    identities.set(token, identity);
  }
  try {
    return buffer.adoptFirstDisplay(
      diagnostics as Readonly<FirstDisplayGptDiagnosticsV1>,
      (traceToken) => identities.get(traceToken)
    )
      ? adoption
      : undefined;
  } catch {
    return undefined;
  }
}

/** Adopt the exact transferred GPT identities without defining, targeting, displaying, or refreshing. */
export function adoptInitialGptSlotsFromHandoff(
  candidate: unknown,
  navigationGeneration: object,
  service: Pick<
    SlotService,
    'adoptCommittedArtifact' | 'adoptGptSlot' | 'adoptRegistrationHighWater'
  >,
  artifactStore: Pick<CommittedArtifactStore, 'current'>,
  targeting: Pick<TargetingService, 'adopt' | 'observePublisherMutations'>,
  adapter: GoogletagAdapter
): PersistentFirstDisplayAdoptionV1 | undefined {
  const adoption = snapshotPersistentFirstDisplayAdoptionV1(candidate);
  if (!adoption) return undefined;
  const slots = frozenArray(dataField(adoption.handoff, 'slots'));
  const cycles = frozenArray(dataField(adoption.handoff, 'cycles'));
  const artifacts = frozenArray(dataField(adoption.handoff, 'artifacts'));
  const highWater = dataField(adoption.handoff, 'highWater');
  const nextSlotRegistrationOrdinal = dataField(highWater, 'nextSlotRegistrationOrdinal');
  if (
    !slots ||
    !cycles ||
    !artifacts ||
    typeof nextSlotRegistrationOrdinal !== 'number' ||
    adoption.identities.length !== cycles.length + artifacts.length
  ) {
    return undefined;
  }
  if (!service.adoptRegistrationHighWater(navigationGeneration, nextSlotRegistrationOrdinal)) {
    return undefined;
  }
  const slotById = new Map<string, unknown>();
  for (const slot of slots) {
    const id = dataField(slot, 'id');
    if (typeof id !== 'string' || slotById.has(id)) return undefined;
    slotById.set(id, slot);
  }
  const adopted = new Set<string>();
  for (let index = 0; index < cycles.length; index += 1) {
    const cycle = cycles[index];
    const slotId = dataField(cycle, 'slotId');
    const placement = typeof slotId === 'string' ? slotById.get(slotId) : undefined;
    const identity = adoption.identities[index];
    const owner = dataField(placement, 'owner');
    const domId = dataField(placement, 'domId');
    const gamPath = dataField(placement, 'gamPath');
    const formats = frozenArray(dataField(placement, 'formats'));
    const targetingOwnership = frozenArray(dataField(placement, 'targetingOwnership'));
    if (
      typeof slotId !== 'string' ||
      adopted.has(slotId) ||
      (owner !== 'publisher' && owner !== 'trusted_server') ||
      typeof identity !== 'object' ||
      identity === null ||
      !targetingOwnership ||
      (owner === 'trusted_server' &&
        (typeof domId !== 'string' || typeof gamPath !== 'string' || !formats))
    ) {
      return undefined;
    }
    const binding: GptSlotBinding =
      owner === 'trusted_server'
        ? {
            definition: {
              adUnitPath: gamPath as string,
              elementId: domId as string,
              sizes: formats as readonly unknown[],
            },
            ownership: owner,
            slot: identity,
          }
        : { ownership: owner, slot: identity };
    const result = service.adoptGptSlot(navigationGeneration, slotId, binding);
    if (!result.ok) return undefined;
    const artifact = artifactStore.current(slotId);
    if (!artifact) return undefined;
    const releases: TargetingOwnership[] = [];
    let observation: ReturnType<TargetingService['observePublisherMutations']> | undefined;
    const releaseTargeting = (): void => {
      for (let releaseIndex = releases.length - 1; releaseIndex >= 0; releaseIndex -= 1) {
        try {
          releases[releaseIndex]?.release();
        } catch {
          // Exact artifact retirement contains hostile GPT targeting cleanup.
        }
      }
      releases.length = 0;
      try {
        observation?.dispose();
      } catch {
        // Adapter observation cleanup cannot restore retired ownership.
      }
      observation = undefined;
    };
    try {
      if (targetingOwnership.length > 0) {
        observation = targeting.observePublisherMutations(identity, adapter);
        if (observation.status !== 'present') {
          releaseTargeting();
          return undefined;
        }
        const boundary = synchronousTargetingBoundary(adapter, identity);
        for (
          let ownershipIndex = 0;
          ownershipIndex < targetingOwnership.length;
          ownershipIndex += 1
        ) {
          const ownership = targetingOwnership[ownershipIndex];
          const key = dataField(ownership, 'key');
          const installed = dataField(ownership, 'installed');
          const prior = frozenArray(dataField(ownership, 'prior'));
          if (
            typeof key !== 'string' ||
            typeof installed !== 'string' ||
            !prior ||
            prior.some((value) => typeof value !== 'string')
          ) {
            releaseTargeting();
            return undefined;
          }
          const adopted = targeting.adopt(
            identity,
            key,
            installed,
            prior as readonly string[],
            artifact.attemptId,
            boundary
          );
          if (!adopted) {
            releaseTargeting();
            return undefined;
          }
          releases.push(adopted);
        }
      }
    } catch {
      releaseTargeting();
      return undefined;
    }
    if (!service.adoptCommittedArtifact(navigationGeneration, slotId, artifact, releaseTargeting)) {
      releaseTargeting();
      return undefined;
    }
    adopted.add(slotId);
  }
  return adoption;
}

export type GptLaterNavigationResult =
  | Readonly<{
      status: 'committed';
      navigationGeneration: object;
      current: true;
    }>
  | Readonly<{
      status: 'rejected';
      navigationGeneration: object;
      current: boolean;
    }>;

export interface GptCapabilityV1 {
  readonly activateLaterLifecycle: () => Readonly<{
    readonly navigate: (path: string) => Promise<GptLaterNavigationResult>;
    readonly release: () => void;
  }>;
  readonly adapter: GoogletagAdapter;
  readonly directAuctionUnitForSlot: (slot: object) => Readonly<object> | undefined;
  readonly installRefreshPolicy: ReturnType<typeof createGptStartup>['installRefreshPolicy'];
  readonly navigation: () => NavigationSession | undefined;
  readonly slots: SlotService;
}

const arrayIsArrayIntrinsic = Array.isArray;
const numberIsFiniteIntrinsic = Number.isFinite;
const objectGetOwnPropertyDescriptorIntrinsic = Object.getOwnPropertyDescriptor;
const objectGetOwnPropertyNamesIntrinsic = Object.getOwnPropertyNames;
const objectGetOwnPropertySymbolsIntrinsic = Object.getOwnPropertySymbols;
const objectIsFrozenIntrinsic = Object.isFrozen;
const jsonParseIntrinsic = JSON.parse;
const promiseThenIntrinsic = Promise.prototype.then;
const reflectApplyIntrinsic = Reflect.apply;
const stringIncludesIntrinsic = String.prototype.includes;
const stringJoinIntrinsic = Array.prototype.join;
const stringSplitIntrinsic = String.prototype.split;
const stringTrimIntrinsic = String.prototype.trim;
const stringCharCodeAtIntrinsic = String.prototype.charCodeAt;
const textEncoder = new TextEncoder();
const textEncoderEncodeIntrinsic = TextEncoder.prototype.encode;

type OwnedBrowserAuctionBidV1 = Extract<
  BrowserAuctionBidV1,
  { readonly rendererReservationId: string }
>;
type PbsCacheBrowserAuctionBidV1 = Extract<
  BrowserAuctionBidV1,
  { readonly renderSource: BaselinePbsCacheSourceV1 }
>;

function validGptTargetingValue(value: string): boolean {
  if (value.length === 0) return false;
  let scalars = 0;
  for (let index = 0; index < value.length; index += 1) {
    const code = reflectApplyIntrinsic(stringCharCodeAtIntrinsic, value, [index]) as number;
    if (code <= 0x1f || code === 0x7f) return false;
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = reflectApplyIntrinsic(stringCharCodeAtIntrinsic, value, [index + 1]) as number;
      if (!(next >= 0xdc00 && next <= 0xdfff)) return false;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return false;
    }
    scalars += 1;
    if (scalars > 40) return false;
  }
  return (
    (reflectApplyIntrinsic(textEncoderEncodeIntrinsic, textEncoder, [value]) as Uint8Array)
      .byteLength <= 160
  );
}

interface ProductionAuctionCapability {
  readonly navigation: NavigationSession;
  readonly projection: Readonly<BrowserAuctionProjectionV1>;
  readonly session: RuntimeSession;
}

interface ProductionSlotsCapability {
  readonly attachPhysicalService: (service: SlotService) => () => void;
}

interface ProductionApsCapability {
  readonly renderPuc: NonNullable<PucBridgeOptions['mountAps']>;
}

interface ProductionRenderCapability {
  readonly attachPucGamAttemptRegistrar: (
    registrar: (input: PucGamAttemptInput) => boolean
  ) => () => void;
  readonly artifacts: Readonly<{
    current: (slot: string) => CommittedRenderArtifact | undefined;
    release: (artifact: CommittedRenderArtifact) => boolean;
  }>;
  readonly bindArtifactRetirement: (
    artifact: CommittedRenderArtifact,
    retire: () => void
  ) => boolean;
  readonly createAttempt: (
    owner: RenderAttemptScope,
    parentAttemptId?: string
  ) => RenderAttemptCreationResult;
  readonly createSlotOperation: (options: SlotOperationOptions) => SlotOperationCreationResult;
  readonly commitPageBids: (
    owner: NavigationSession,
    slotRegistry: ReturnType<SlotService['projectionRegistry']>,
    candidate: unknown
  ) => boolean;
  readonly mintLifecycleTicket: () => IdentityGenerationResult<string>;
  readonly renderWinner: (attempt: RenderAttempt) => boolean;
  readonly reservations: ReservationService;
}

interface ProductionMessagesCapability {
  readonly messaging: Parameters<typeof createPucBridge>[0]['messaging'];
}

interface ProductionTraceCapability {
  readonly observations: Readonly<{
    publish: (observation: Readonly<Record<string, unknown>>) => boolean;
  }>;
}

interface InitialProjectionServices {
  readonly googletag: GoogletagAdapter;
  readonly projection: Readonly<BrowserAuctionProjectionV1>;
  readonly navigation: NavigationSession;
  readonly protect: RuntimeCapabilityV1['protectFirstDisplayAttemptBatch'];
  readonly pucBridge: Pick<PucBridge, 'recordNonemptyGam' | 'registerGamAttempt'>;
  readonly render: ProductionRenderCapability;
  readonly slots: SlotService;
  readonly targeting: TargetingService;
  readonly requestClass?: string;
}

export interface GptSlotOperationInput extends Omit<PucGamAttemptInput, 'attempt'> {
  readonly attempt: RenderAttempt;
  readonly createFallback?: SlotOperationOptions['createFallback'];
  readonly createSlotOperation: (options: SlotOperationOptions) => SlotOperationCreationResult;
  readonly operation: 'display' | 'refresh';
  readonly pucBridge: Pick<PucBridge, 'recordNonemptyGam' | 'registerGamAttempt'>;
  readonly requestClass: string;
  readonly slots: Pick<SlotService, 'request'>;
}

export type GptWinnerPublicationFailureReason = Extract<
  RenderFailureReason,
  | 'descriptor_invalid'
  | 'gpt_request_failed'
  | 'registry_full'
  | 'reservation_collision'
  | 'slot_unresolved'
  | 'winner_not_renderable'
>;

export type GptWinnerPublicationResult =
  | Extract<SlotOperationCreationResult, { ok: true }>
  | Readonly<{ ok: false; reason: GptWinnerPublicationFailureReason }>;

export interface GptWinnerPublicationInput extends Omit<
  GptSlotOperationInput,
  'artifact' | 'pucBridge' | 'reservationId' | 'slots'
> {
  readonly artifact: CommittedRenderArtifact;
  readonly bid: OwnedBrowserAuctionBidV1;
  readonly googletag: GoogletagAdapter;
  readonly navigation: NavigationSession;
  readonly placement: BrowserAuctionSlotV1;
  readonly pucBridge: Pick<PucBridge, 'recordNonemptyGam' | 'registerGamAttempt'>;
  readonly reservations: Pick<ReservationService, 'registerRender' | 'tombstone'>;
  readonly slot: object;
  readonly slots: Pick<SlotService, 'isBoundGptSlot' | 'request'>;
  readonly targeting: Pick<TargetingService, 'observePublisherMutations' | 'own'>;
}

function currentProjectedWinner(input: GptWinnerPublicationInput): boolean {
  try {
    const projection = input.navigation.currentAuctionProjection as
      BrowserAuctionProjectionV1 | undefined;
    const bid = input.bid;
    const placement = input.placement;
    if (
      !projection ||
      !objectIsFrozenIntrinsic(projection) ||
      !objectIsFrozenIntrinsic(bid) ||
      !objectIsFrozenIntrinsic(bid.renderSource) ||
      !objectIsFrozenIntrinsic(bid.targeting) ||
      !objectIsFrozenIntrinsic(placement) ||
      !objectIsFrozenIntrinsic(placement.formats) ||
      !objectIsFrozenIntrinsic(placement.targeting) ||
      bid.slot !== input.attempt.slot ||
      placement.slot !== bid.slot ||
      input.attempt.navigationGeneration !== input.navigation.generation ||
      input.owner.id !== input.attempt.id ||
      input.owner.slot !== input.attempt.slot ||
      input.owner.generation !== input.attempt.generation ||
      input.owner.navigationGeneration !== input.navigation.generation ||
      input.artifact.kind !== 'puc' ||
      input.artifact.attemptId !== input.attempt.id ||
      input.artifact.slot !== input.attempt.slot ||
      input.artifact.navigationGeneration !== input.navigation.generation ||
      typeof input.artifact.dispose !== 'function' ||
      typeof input.slot !== 'object' ||
      input.slot === null ||
      !input.navigation.isCurrent() ||
      input.attempt.snapshot().outcome !== undefined
    ) {
      return false;
    }
    let exactBid = false;
    for (let index = 0; index < projection.bids.length; index += 1) {
      if (projection.bids[index] === bid) {
        if (exactBid) return false;
        exactBid = true;
      }
    }
    if (!exactBid) return false;
    let exactPlacement = false;
    for (let index = 0; index < projection.slots.length; index += 1) {
      if (projection.slots[index] === placement) {
        if (exactPlacement) return false;
        exactPlacement = true;
      }
    }
    if (!exactPlacement) return false;
    let exactWinner = false;
    for (let index = 0; index < projection.auction.results.length; index += 1) {
      const result = projection.auction.results[index];
      if (
        result?.outcome === 'winner' &&
        result.slot === bid.slot &&
        result.candidateId === bid.candidateId
      ) {
        if (exactWinner) return false;
        exactWinner = true;
      }
    }
    return exactWinner;
  } catch {
    return false;
  }
}

function targetingEntries(
  bid: BrowserAuctionBidV1,
  placement: BrowserAuctionSlotV1
): readonly (readonly [string, string])[] | undefined {
  try {
    const bidNames = objectGetOwnPropertyNamesIntrinsic(bid.targeting);
    const placementNames = objectGetOwnPropertyNamesIntrinsic(placement.targeting);
    if (
      bidNames.length > 32 ||
      placementNames.length > 32 ||
      objectGetOwnPropertySymbolsIntrinsic(bid.targeting).length !== 0 ||
      objectGetOwnPropertySymbolsIntrinsic(placement.targeting).length !== 0
    ) {
      return undefined;
    }
    const names: string[] = [];
    const insertNames = (source: readonly string[]): boolean => {
      for (let index = 0; index < source.length; index += 1) {
        const name = source[index];
        if (!name || name === 'hb_adid' || name === 'hb_cache_host' || name === 'hb_cache_path') {
          return false;
        }
        let insertion = 0;
        while (insertion < names.length && (names[insertion] as string) < name) insertion += 1;
        if (names[insertion] === name) continue;
        for (let move = names.length; move > insertion; move -= 1) {
          names[move] = names[move - 1] as string;
        }
        names[insertion] = name;
      }
      return true;
    };
    if (!insertNames(placementNames) || !insertNames(bidNames)) return undefined;
    const entries: Array<readonly [string, string]> = !('rendererReservationId' in bid)
      ? [
          Object.freeze(['hb_adid', bid.renderSource.cacheId]),
          Object.freeze(['hb_cache_host', bid.renderSource.cacheHost]),
          Object.freeze(['hb_cache_path', bid.renderSource.cachePath]),
        ]
      : [Object.freeze(['hb_adid', bid.rendererReservationId])];
    for (let index = 0; index < entries.length; index += 1) {
      const entry = entries[index];
      if (!entry || !validGptTargetingValue(entry[1])) return undefined;
    }
    for (let index = 0; index < names.length; index += 1) {
      const key = names[index];
      if (!key) return undefined;
      const bidDescriptor = objectGetOwnPropertyDescriptorIntrinsic(bid.targeting, key);
      const placementDescriptor = objectGetOwnPropertyDescriptorIntrinsic(placement.targeting, key);
      const descriptor = bidDescriptor ?? placementDescriptor;
      if (
        !descriptor ||
        !descriptor.enumerable ||
        !('value' in descriptor) ||
        typeof descriptor.value !== 'string'
      ) {
        return undefined;
      }
      entries[entries.length] = Object.freeze([key, descriptor.value]);
    }
    return Object.freeze(entries);
  } catch {
    return undefined;
  }
}

function synchronousTargetingBoundary(adapter: GoogletagAdapter, slot: object): TargetingBoundary {
  const invoke = <Value>(command: (gpt: Readonly<GoogletagFacade>) => Value): Value => {
    let completed = false;
    let failed = false;
    let value: Value | undefined;
    let failure: unknown;
    const operation = adapter.run((gpt) => {
      try {
        value = command(gpt);
        return value;
      } catch (error) {
        failed = true;
        failure = error;
        throw error;
      } finally {
        completed = true;
      }
    });
    void reflectApplyIntrinsic(promiseThenIntrinsic, operation.result, [
      () => undefined,
      () => undefined,
    ]);
    if (!completed) {
      operation.dispose();
      throw new Error('GPT targeting operation is not synchronously available');
    }
    if (failed) throw failure;
    return value as Value;
  };
  return Object.freeze({
    clearTargeting: (key?: string) => invoke((gpt) => gpt.clearTargeting(slot, key)),
    getTargeting: (key: string) => invoke((gpt) => gpt.getTargeting(slot, key)),
    setTargeting: (key: string, value: string | readonly string[]) =>
      invoke((gpt) => gpt.setTargeting(slot, key, value)),
  });
}

function reservationFailure(reason: string): GptWinnerPublicationFailureReason {
  if (reason === 'reservation_collision') return 'reservation_collision';
  if (reason === 'registry_full') return 'registry_full';
  if (reason === 'invalid_render_source' || reason === 'invalid_reservation_id') {
    return 'descriptor_invalid';
  }
  return 'gpt_request_failed';
}

/** Publish one server-projected PUC winner without exposing capability state out of order. */
export async function publishGptWinner(
  input: GptWinnerPublicationInput
): Promise<GptWinnerPublicationResult> {
  const failAttempt = (reason: GptWinnerPublicationFailureReason): GptWinnerPublicationResult => {
    try {
      input.attempt.fail(reason);
    } catch {
      // The attempt latch remains authoritative.
    }
    return Object.freeze({ ok: false, reason });
  };
  const disposeArtifact = (): void => {
    try {
      input.artifact.dispose();
    } catch {
      // Rejected publication retains no artifact authority.
    }
  };
  if (!currentProjectedWinner(input)) {
    disposeArtifact();
    return failAttempt('winner_not_renderable');
  }
  const isStillBound = (): boolean => {
    try {
      return input.slots.isBoundGptSlot(input.navigation.generation, input.bid.slot, input.slot);
    } catch {
      return false;
    }
  };
  if (!isStillBound()) {
    disposeArtifact();
    return failAttempt('slot_unresolved');
  }
  const entries = targetingEntries(input.bid, input.placement);
  if (!entries) {
    disposeArtifact();
    return failAttempt('descriptor_invalid');
  }
  const winnerContext = Object.freeze({ selectedCpm: input.bid.cpm });
  const registration = (() => {
    try {
      return input.reservations.registerRender({
        reservationId: input.bid.rendererReservationId,
        slot: input.bid.slot,
        navigation: input.navigation,
        attemptId: input.attempt.id,
        renderSource: input.bid.renderSource,
        winnerContext,
      });
    } catch {
      return Object.freeze({ ok: false as const, reason: 'service_disposed' as const });
    }
  })();
  if (!registration.ok) {
    disposeArtifact();
    return failAttempt(reservationFailure(registration.reason));
  }

  const owners: TargetingOwnership[] = [];
  let observation: ReturnType<TargetingService['observePublisherMutations']> | undefined;
  let retirementAttempted = false;
  let resourcesDisposed = false;
  const tombstone = (): void => {
    if (retirementAttempted) return;
    retirementAttempted = true;
    try {
      input.reservations.tombstone(
        {
          reservationId: input.bid.rendererReservationId,
          slot: input.bid.slot,
          navigationGeneration: input.navigation.generation,
          attemptId: input.attempt.id,
        },
        'disposed'
      );
    } catch {
      // Runtime disposal retains the last-resort retirement boundary.
    }
  };
  const disposeResources = (): void => {
    if (resourcesDisposed) return;
    resourcesDisposed = true;
    tombstone();
    for (let index = owners.length - 1; index >= 0; index -= 1) {
      try {
        owners[index]?.release();
      } catch {
        // One targeting cleanup cannot suppress the remaining rollback.
      }
    }
    try {
      observation?.dispose();
    } catch {
      // The adapter owns final wrapper restoration.
    }
    disposeArtifact();
  };
  try {
    observation = input.targeting.observePublisherMutations(input.slot, input.googletag);
    await observation.result;
    if (!input.navigation.isCurrent() || input.attempt.snapshot().outcome !== undefined) {
      throw new Error('stale GPT publication');
    }
    if (!isStillBound()) {
      disposeResources();
      return failAttempt('slot_unresolved');
    }
    const boundary = synchronousTargetingBoundary(input.googletag, input.slot);
    for (let index = 0; index < entries.length; index += 1) {
      const entry = entries[index];
      if (!entry) throw new Error('targeting entry unavailable');
      const owner = input.targeting.own(input.slot, entry[0], entry[1], input.attempt.id, boundary);
      if (!owner) throw new Error('targeting ownership unavailable');
      owners[owners.length] = owner;
    }
    if (!input.navigation.isCurrent() || input.attempt.snapshot().outcome !== undefined) {
      throw new Error('stale GPT publication');
    }
    if (!isStillBound()) {
      disposeResources();
      return failAttempt('slot_unresolved');
    }
  } catch {
    disposeResources();
    return failAttempt('gpt_request_failed');
  }

  const publishedArtifact = Object.freeze({
    kind: 'puc' as const,
    attemptId: input.artifact.attemptId,
    slot: input.artifact.slot,
    navigationGeneration: input.artifact.navigationGeneration,
    dispose: disposeResources,
  });
  let bridgeRegistered = false;
  let requestStarted = false;
  let operation: SlotOperationCreationResult;
  try {
    operation = startGptSlotOperation({
      artifact: publishedArtifact,
      attempt: input.attempt,
      ...(input.createFallback === undefined ? {} : { createFallback: input.createFallback }),
      createSlotOperation: input.createSlotOperation,
      operation: input.operation,
      owner: input.owner,
      pucBridge: {
        registerGamAttempt: (bridgeInput) => {
          bridgeRegistered = input.pucBridge.registerGamAttempt(bridgeInput) === true;
          return bridgeRegistered;
        },
        recordNonemptyGam: (bridgeInput) => input.pucBridge.recordNonemptyGam(bridgeInput),
      },
      requestClass: input.requestClass,
      reservationId: input.bid.rendererReservationId,
      slots: {
        request: (requestInput) => {
          const handle = input.slots.request({ ...requestInput, expectedSlot: input.slot });
          requestStarted = true;
          return handle;
        },
      },
    });
  } catch {
    disposeResources();
    return failAttempt('gpt_request_failed');
  }
  if (!operation.ok || !bridgeRegistered || !requestStarted) {
    disposeResources();
    return failAttempt('gpt_request_failed');
  }
  return operation;
}

interface PbsCacheGptPublicationInput {
  readonly bid: PbsCacheBrowserAuctionBidV1;
  readonly binding: Readonly<{ operation: 'display' | 'refresh'; slot: object }>;
  readonly googletag: GoogletagAdapter;
  readonly navigation: NavigationSession;
  readonly placement: BrowserAuctionSlotV1;
  readonly requestClass: string;
  readonly slots: Pick<SlotService, 'isBoundGptSlot' | 'request'>;
  readonly targeting: Pick<TargetingService, 'observePublisherMutations' | 'own'>;
}

async function publishPbsCacheGptWinner(input: PbsCacheGptPublicationInput): Promise<void> {
  const projection = input.navigation.currentAuctionProjection as
    Readonly<BrowserAuctionProjectionV1> | undefined;
  if (!projection) return;
  const current = (): boolean => {
    try {
      if (
        !input.navigation.isCurrent() ||
        input.navigation.currentAuctionProjection !== projection
      ) {
        return false;
      }
      let bids = 0;
      let slots = 0;
      let winners = 0;
      for (let index = 0; index < projection.bids.length; index += 1) {
        if (projection.bids[index] === input.bid) bids += 1;
      }
      for (let index = 0; index < projection.slots.length; index += 1) {
        if (projection.slots[index] === input.placement) slots += 1;
      }
      for (let index = 0; index < projection.auction.results.length; index += 1) {
        const decision = projection.auction.results[index];
        if (
          decision?.outcome === 'winner' &&
          decision.slot === input.bid.slot &&
          decision.candidateId === input.bid.candidateId
        ) {
          winners += 1;
        }
      }
      return (
        bids === 1 &&
        slots === 1 &&
        winners === 1 &&
        input.slots.isBoundGptSlot(input.navigation.generation, input.bid.slot, input.binding.slot)
      );
    } catch {
      return false;
    }
  };
  if (!current()) return;
  const entries = targetingEntries(input.bid, input.placement);
  if (!entries) return;

  const ownerId = `pbs-cache:${projection.auction.auctionId}:${input.bid.candidateId}`;
  const owners: TargetingOwnership[] = [];
  let observation: ReturnType<TargetingService['observePublisherMutations']> | undefined;
  let handle: ReturnType<SlotService['request']> | undefined;
  try {
    observation = input.targeting.observePublisherMutations(input.binding.slot, input.googletag);
    await observation.result;
    if (!current()) return;
    const boundary = synchronousTargetingBoundary(input.googletag, input.binding.slot);
    for (let index = 0; index < entries.length; index += 1) {
      const entry = entries[index];
      if (!entry) return;
      const ownership = input.targeting.own(
        input.binding.slot,
        entry[0],
        entry[1],
        ownerId,
        boundary
      );
      if (!ownership) return;
      owners.push(ownership);
    }
    if (!current()) return;
    handle = input.slots.request({
      expectedSlot: input.binding.slot,
      intentId: ownerId,
      navigationGeneration: input.navigation.generation,
      operation: input.binding.operation,
      registeredSlotId: input.bid.slot,
      requestClass: input.requestClass,
    });
    await handle.result;
  } catch (error) {
    if (input.navigation.isCurrent()) log.warn('GPT PBS Cache publication failed', error);
  } finally {
    try {
      handle?.dispose();
    } catch {
      // Request settlement remains authoritative.
    }
    for (let index = owners.length - 1; index >= 0; index -= 1) {
      try {
        owners[index]?.release();
      } catch {
        // One targeting cleanup cannot suppress the remaining rollback.
      }
    }
    try {
      observation?.dispose();
    } catch {
      // The adapter owns final wrapper restoration.
    }
  }
}

function settleFromSlotOutcome(
  attempt: RenderAttempt,
  bridge: GptSlotOperationInput['pucBridge'],
  bridgeInput: PucGamAttemptInput,
  outcome: SlotRequestOutcome
): void {
  try {
    if (outcome.status === 'empty') {
      attempt.fail('gam_empty');
      return;
    }
    if (outcome.status === 'rendered') {
      if (!bridge.recordNonemptyGam(bridgeInput)) attempt.fail('cycle_unattributable');
      return;
    }
    if (outcome.status === 'failed') {
      attempt.fail(outcome.reason);
      return;
    }
    if (outcome.status === 'cancelled') attempt.cancel(outcome.reason);
  } catch {
    try {
      attempt.fail('internal_error');
    } catch {
      // The attempt latch remains the terminal authority.
    }
  }
}

/**
 * Join one TS-owned physical GPT cycle to its primary render attempt.
 *
 * Only the slot service may identify an attributable empty cycle. The resulting
 * `gam_empty` transition is therefore the sole path that can activate the
 * optional `SlotOperation` fallback child.
 */
export function startGptSlotOperation(input: GptSlotOperationInput): SlotOperationCreationResult {
  const operation = input.createSlotOperation({
    primary: input.attempt,
    ...(input.createFallback === undefined ? {} : { createFallback: input.createFallback }),
  });
  if (!operation.ok) return operation;

  const bridgeInput = Object.freeze({
    artifact: input.artifact,
    attempt: input.attempt,
    owner: input.owner,
    reservationId: input.reservationId,
  });
  const registered = (() => {
    try {
      return input.pucBridge.registerGamAttempt(bridgeInput);
    } catch {
      return false;
    }
  })();
  if (!registered) {
    try {
      input.attempt.fail('gpt_request_failed');
    } catch {
      // The operation still observes any terminal result already committed by the bridge.
    }
    return operation;
  }

  let handle: ReturnType<SlotService['request']>;
  try {
    handle = input.slots.request({
      intentId: input.attempt.id,
      navigationGeneration: input.attempt.navigationGeneration,
      operation: input.operation,
      registeredSlotId: input.attempt.slot,
      requestClass: input.requestClass,
    });
  } catch {
    input.attempt.fail('gpt_request_failed');
    return operation;
  }

  let handleDisposed = false;
  const disposeHandle = (): void => {
    if (handleDisposed) return;
    handleDisposed = true;
    try {
      handle.dispose();
    } catch {
      // Attempt settlement remains authoritative when request cleanup throws.
    }
  };
  const observing = (() => {
    try {
      return input.attempt.onSettled(disposeHandle);
    } catch {
      return false;
    }
  })();
  if (!observing) {
    disposeHandle();
    try {
      input.attempt.fail('internal_error');
    } catch {
      // A concurrently terminal attempt cannot be overwritten.
    }
    return operation;
  }

  void handle.result.then(
    (outcome) => settleFromSlotOutcome(input.attempt, input.pucBridge, bridgeInput, outcome),
    () => {
      try {
        input.attempt.fail('gpt_request_failed');
      } catch {
        // A late rejected request cannot overwrite an existing terminal outcome.
      }
    }
  );
  return operation;
}

function exactCapability<Value extends object>(
  interfaces: Readonly<Record<string, unknown>>,
  key: string
): Value | undefined {
  const candidate = interfaces[key];
  return typeof candidate === 'object' && candidate !== null && Object.isFrozen(candidate)
    ? (candidate as Value)
    : undefined;
}

function gptDiagnosticsActive(runtime: RuntimeCapabilityV1): boolean {
  try {
    const boot = runtime.boot();
    if (!boot) return false;
    const diagnostics = Object.getOwnPropertyDescriptor(boot, 'diagnostics');
    if (!diagnostics || !('value' in diagnostics)) return false;
    const gpt = Object.getOwnPropertyDescriptor(diagnostics.value, 'gpt');
    if (!gpt || !('value' in gpt)) return false;
    const active = Object.getOwnPropertyDescriptor(gpt.value, 'active');
    return Boolean(active && 'value' in active && active.value === true);
  } catch {
    return false;
  }
}

function resolveProjectedSlotElement(
  document: Document,
  placement: Readonly<BrowserAuctionSlotV1>
): HTMLElement | undefined {
  try {
    const exact = document.getElementById(placement.divId);
    if (exact instanceof HTMLElement) return exact;
    const matches = [...document.querySelectorAll<HTMLElement>('[id]')].filter(
      (element) => element.id.startsWith(placement.divId) && !element.id.endsWith('-container')
    );
    return matches.length === 1 ? matches[0] : undefined;
  } catch {
    return undefined;
  }
}

const TS_DISPLAY_RENDERER =
  '(function(){window.render=function(d,h,w){' +
  'var f=h.mkFrame(w.document,{width:d.width||"100%",height:d.height||"100%"});' +
  'if(d.adUrl&&!d.ad){f.src=d.adUrl;}else{f.srcdoc=d.ad;}' +
  'w.document.body.appendChild(f);};})();';

const AUCTION_PRICE_MACRO = '${AUCTION_PRICE}';

interface CachedPbsBid {
  readonly adm: string;
  readonly width?: number;
  readonly height?: number;
  readonly price?: number;
}

function expandCachedAuctionPriceMacro(markup: string, cpm: number): string {
  if (!(reflectApplyIntrinsic(stringIncludesIntrinsic, markup, [AUCTION_PRICE_MACRO]) as boolean)) {
    return markup;
  }
  const pieces = reflectApplyIntrinsic(stringSplitIntrinsic, markup, [
    AUCTION_PRICE_MACRO,
  ]) as string[];
  return reflectApplyIntrinsic(stringJoinIntrinsic, pieces, [String(cpm)]) as string;
}

/** Preserve current-main PBS Cache decoding inside GPT without publishing a cache service. */
function parseCachedPbsBid(body: string): CachedPbsBid | undefined {
  let parsed: unknown;
  try {
    parsed = reflectApplyIntrinsic(jsonParseIntrinsic, JSON, [body]) as unknown;
  } catch {
    const trimmed = reflectApplyIntrinsic(stringTrimIntrinsic, body, []) as string;
    return trimmed.length > 0 ? Object.freeze({ adm: body }) : undefined;
  }
  if (!parsed || typeof parsed !== 'object' || arrayIsArrayIntrinsic(parsed)) return undefined;
  const record = parsed as Record<string, unknown>;
  if (typeof record.adm !== 'string' || record.adm.length === 0) return undefined;
  const finite = (value: unknown): number | undefined =>
    typeof value === 'number' && numberIsFiniteIntrinsic(value) ? value : undefined;
  const dimension = (value: unknown): number | undefined => {
    const numeric = finite(value);
    return numeric !== undefined && numeric > 0 ? numeric : undefined;
  };
  const width = dimension(record.w) ?? dimension(record.width);
  const height = dimension(record.h) ?? dimension(record.height);
  const price = finite(record.price);
  return Object.freeze({
    adm: record.adm,
    ...(width === undefined ? {} : { width }),
    ...(height === undefined ? {} : { height }),
    ...(price === undefined ? {} : { price }),
  });
}

type PbsCacheBridgeFailure =
  'cache_fetch_failed' | 'invalid_cache_payload' | 'response_post_failed';

/** Install the activation-scoped current-main PBS Cache bridge owned only by GPT. */
export function installPbsCacheBridge(
  document: Document,
  auction: ProductionAuctionCapability,
  isActive: () => boolean,
  observe: (observation: Readonly<Record<string, unknown>>) => boolean
): () => void {
  const view = document.defaultView;
  if (!view) throw new TypeError('GPT PBS Cache bridge requires a browser window');
  const inFlight = new WeakMap<object, Set<string>>();
  let listening = true;

  const publishFailure = (slot: string, reason: PbsCacheBridgeFailure): void => {
    try {
      observe(Object.freeze({ kind: 'pbs_cache_bridge', slotId: slot, reason }));
    } catch {
      // Diagnostics cannot alter cache bridge ownership.
    }
  };
  const parseRequest = (candidate: unknown): Readonly<{ adId: string }> | undefined => {
    try {
      const value =
        typeof candidate === 'string'
          ? (reflectApplyIntrinsic(jsonParseIntrinsic, JSON, [candidate]) as unknown)
          : candidate;
      if (typeof value !== 'object' || value === null || arrayIsArrayIntrinsic(value)) {
        return undefined;
      }
      const record = value as Record<string, unknown>;
      return record.message === 'Prebid Request' &&
        typeof record.adId === 'string' &&
        record.adId !== ''
        ? Object.freeze({ adId: record.adId })
        : undefined;
    } catch {
      return undefined;
    }
  };
  const sourceOwnsPlacement = (
    source: MessageEventSource | null,
    placement: Readonly<BrowserAuctionSlotV1>
  ): boolean => {
    if (!source) return false;
    try {
      const exact = document.getElementById(placement.divId);
      const configuredContainer = document.getElementById(`${placement.divId}-container`);
      const owns = (root: HTMLElement): boolean => {
        const frames = root.querySelectorAll('iframe');
        for (let index = 0; index < frames.length; index += 1) {
          if (frames.item(index)?.contentWindow === source) return true;
        }
        return false;
      };
      return (
        (exact instanceof HTMLElement && owns(exact)) ||
        (configuredContainer instanceof HTMLElement &&
          configuredContainer !== exact &&
          owns(configuredContainer))
      );
    } catch {
      return false;
    }
  };
  const listener = (event: MessageEvent): void => {
    if (!listening || !isActive()) return;
    const request = parseRequest(event.data);
    const port = event.ports?.[0];
    if (!request || !port || typeof port.postMessage !== 'function') return;
    const navigation = auction.session.currentNavigation;
    const projection = navigation?.currentAuctionProjection as
      Readonly<BrowserAuctionProjectionV1> | undefined;
    if (!navigation?.isCurrent() || !projection) return;
    let placement: BrowserAuctionSlotV1 | undefined;
    for (let index = 0; index < projection.slots.length; index += 1) {
      const candidate = projection.slots[index];
      if (!candidate || !sourceOwnsPlacement(event.source, candidate)) continue;
      if (placement) return;
      placement = candidate;
    }
    if (!placement) return;
    let bid: PbsCacheBrowserAuctionBidV1 | undefined;
    for (let index = 0; index < projection.bids.length; index += 1) {
      const candidate = projection.bids[index];
      if (
        !candidate ||
        candidate.slot !== placement.slot ||
        candidate.renderSource.type !== 'pbs_cache' ||
        candidate.renderSource.cacheId !== request.adId
      ) {
        continue;
      }
      if (bid) return;
      bid = candidate as PbsCacheBrowserAuctionBidV1;
    }
    if (!bid) return;

    event.stopImmediatePropagation();
    const key = `${bid.slot}\u0000${request.adId}`;
    let generationFlights = inFlight.get(navigation.generation);
    if (!generationFlights) {
      generationFlights = new Set<string>();
      inFlight.set(navigation.generation, generationFlights);
    }
    if (generationFlights.has(key)) return;
    generationFlights.add(key);

    const remainsCurrent = (): boolean =>
      listening &&
      isActive() &&
      navigation.isCurrent() &&
      auction.session.currentNavigation === navigation &&
      navigation.currentAuctionProjection === projection &&
      (() => {
        for (let index = 0; index < projection.bids.length; index += 1) {
          if (projection.bids[index] === bid) return true;
        }
        return false;
      })();
    const cacheUrl = `https://${bid.renderSource.cacheHost}${bid.renderSource.cachePath}?uuid=${encodeURIComponent(request.adId)}`;
    const complete = async (): Promise<void> => {
      try {
        const fetcher = globalThis.fetch;
        if (typeof fetcher !== 'function') throw new Error('fetch unavailable');
        const response = await fetcher(cacheUrl, { mode: 'cors' });
        if (!response.ok) throw new Error(`cache HTTP ${response.status}`);
        const body = await response.text();
        if (!remainsCurrent()) return;
        const cached = parseCachedPbsBid(body);
        if (!cached) {
          publishFailure(bid.slot, 'invalid_cache_payload');
          return;
        }
        const width = cached.width ?? (bid.renderSource.width || placement.formats[0]?.[0] || 728);
        const height =
          cached.height ?? (bid.renderSource.height || placement.formats[0]?.[1] || 90);
        const adm =
          cached.price === undefined
            ? cached.adm
            : expandCachedAuctionPriceMacro(cached.adm, cached.price);
        try {
          port.postMessage(
            JSON.stringify({
              message: 'Prebid Response',
              adId: request.adId,
              ad: adm,
              renderer: TS_DISPLAY_RENDERER,
              width,
              height,
            })
          );
        } catch {
          publishFailure(bid.slot, 'response_post_failed');
          return;
        }
        if (!remainsCurrent()) return;
        resizeCollapsedPucShell({ source: event.source as object, width, height });
      } catch {
        if (remainsCurrent()) publishFailure(bid.slot, 'cache_fetch_failed');
      } finally {
        generationFlights?.delete(key);
      }
    };
    void complete();
  };

  view.addEventListener('message', listener);
  return (): void => {
    if (!listening) return;
    listening = false;
    view.removeEventListener('message', listener);
  };
}

function terminalLatch(attempt: RenderAttempt): Promise<unknown> {
  return new Promise((resolve) => {
    if (!attempt.onSettled(resolve)) resolve(attempt.snapshot().outcome);
  });
}

interface PreparedInitialGptWinner {
  readonly attempt: RenderAttempt;
  readonly bid: OwnedBrowserAuctionBidV1;
  readonly binding: Readonly<{ operation: 'display' | 'refresh'; slot: object }> | undefined;
  readonly decision: Readonly<{ slot: string; outcome: 'winner'; candidateId: string }>;
  readonly owner: RenderAttemptScope;
  readonly placement: BrowserAuctionSlotV1;
  readonly terminal: Promise<unknown>;
}

/** Start one immutable initial GPT winner batch and protect every terminal latch together. */
export async function publishInitialGptProjection(
  document: Document,
  input: InitialProjectionServices
): Promise<void> {
  const { googletag, navigation, projection, render, slots } = input;
  if (!navigation.isCurrent() || projection.slots.length === 0) return;
  const physicalBySlot = new Map<
    string,
    Readonly<{ operation: 'display' | 'refresh'; slot: object }>
  >();
  const operation = googletag.run(
    (gpt) => {
      for (let index = 0; index < projection.slots.length; index += 1) {
        const placement = projection.slots[index];
        if (!placement || !navigation.isCurrent()) break;
        const element = resolveProjectedSlotElement(document, placement);
        if (!element) continue;
        const definition = Object.freeze({
          adUnitPath: placement.gamUnitPath,
          elementId: element.id,
          sizes: placement.formats,
        });
        const existing = gpt.slots().filter((slot) => gpt.slotElementId?.(slot) === element.id);
        if (existing.length > 1) continue;
        const publisherSlot = existing[0];
        if (publisherSlot) {
          const adopted = slots.adoptGptSlot(navigation.generation, placement.slot, {
            definition,
            elementIdPrefix: placement.divId,
            ownership: 'publisher',
            slot: publisherSlot,
          });
          if (adopted.ok) {
            physicalBySlot.set(
              placement.slot,
              Object.freeze({ operation: 'refresh', slot: publisherSlot })
            );
          }
          continue;
        }
        const defined = gpt.transactionalDefine(
          definition,
          () => navigation.isCurrent(),
          (candidate) => {
            let committed = false;
            return Object.freeze({
              commit: (): boolean => {
                const adopted = slots.adoptGptSlot(navigation.generation, placement.slot, {
                  definition,
                  elementIdPrefix: placement.divId,
                  ownership: 'trusted_server',
                  slot: candidate,
                });
                committed = adopted.ok;
                return committed;
              },
              rollback: (): void => {
                if (!committed) return;
                committed = false;
                slots.recordPublisherDestruction(candidate);
              },
            });
          }
        );
        if (defined.status === 'defined') {
          physicalBySlot.set(
            placement.slot,
            Object.freeze({ operation: 'display', slot: defined.slot })
          );
        }
      }
    },
    { signal: navigation.signal }
  );
  try {
    await operation.result;
  } catch (error) {
    if (navigation.isCurrent()) log.warn('GPT projection slot binding failed', error);
  }
  if (!navigation.isCurrent()) return;

  const batch = navigation.createAuctionBatch(`gpt:${projection.auction.auctionId}`);
  if (!batch) return;
  const prepared: PreparedInitialGptWinner[] = [];
  const cachePublications: Promise<void>[] = [];
  let winnerIndex = 0;
  for (let index = 0; index < projection.auction.results.length; index += 1) {
    const decision = projection.auction.results[index];
    const placement = projection.slots[index];
    if (!decision || !placement || decision.outcome !== 'winner') continue;
    const bid = projection.bids[winnerIndex];
    winnerIndex += 1;
    if (!bid || !navigation.isCurrent()) continue;
    const binding = physicalBySlot.get(decision.slot);
    if (!('rendererReservationId' in bid)) {
      if (binding) {
        cachePublications.push(
          publishPbsCacheGptWinner({
            bid,
            binding,
            googletag,
            navigation,
            placement,
            requestClass: input.requestClass ?? 'initial',
            slots,
            targeting: input.targeting,
          })
        );
      }
      continue;
    }
    const owner = batch.createRenderAttempt(decision.slot);
    if (!owner.ok) continue;
    const created = render.createAttempt(owner.value);
    if (!created.ok) continue;
    prepared.push(
      Object.freeze({
        attempt: created.value,
        bid,
        binding,
        decision,
        owner: owner.value,
        placement,
        terminal: terminalLatch(created.value),
      })
    );
  }
  if (prepared.length === 0 && cachePublications.length === 0) return;
  input.protect(Object.freeze([...prepared.map(({ terminal }) => terminal), ...cachePublications]));

  await Promise.all([
    ...cachePublications,
    ...prepared.map(async ({ attempt, bid, binding, decision, owner, placement }) => {
      if (!binding) {
        attempt.fail('slot_unresolved');
        return;
      }
      const artifact = Object.freeze({
        kind: 'puc' as const,
        attemptId: attempt.id,
        slot: attempt.slot,
        navigationGeneration: attempt.navigationGeneration,
        dispose: () => undefined,
      });
      const published = await publishGptWinner({
        artifact,
        attempt,
        bid,
        createSlotOperation: render.createSlotOperation,
        googletag,
        navigation,
        operation: binding.operation,
        owner,
        placement,
        pucBridge: input.pucBridge,
        requestClass: input.requestClass ?? 'initial',
        reservations: render.reservations,
        slot: binding.slot,
        slots,
        targeting: input.targeting,
        createFallback: (parentAttemptId) => {
          const fallbackOwner = batch.createRenderAttempt(decision.slot);
          if (!fallbackOwner.ok) {
            return Object.freeze({
              ok: false as const,
              reason:
                fallbackOwner.reason === 'identity_generation_failed'
                  ? ('identity_generation_failed' as const)
                  : fallbackOwner.reason === 'stale_owner'
                    ? ('stale_owner' as const)
                    : ('invalid_attempt' as const),
            });
          }
          const fallback = render.createAttempt(fallbackOwner.value, parentAttemptId);
          if (!fallback.ok) return fallback;
          if (
            !fallback.value.admitDirectWinner(
              bid.renderSource,
              Object.freeze({ selectedCpm: bid.cpm })
            )
          ) {
            fallback.value.fail('winner_not_renderable');
            return fallback;
          }
          if (!render.renderWinner(fallback.value)) fallback.value.fail('winner_not_renderable');
          return fallback;
        },
      });
      if (!published.ok && navigation.isCurrent()) {
        log.warn('GPT projection winner publication failed', published.reason);
      }
    }),
  ]);
}

function prepareProductionGpt(context: IntegrationPrepareContext): PreparedIntegration {
  const runtime = exactCapability<RuntimeCapabilityV1>(context.interfaces, 'runtime.v1');
  if (!runtime) throw new TypeError('GPT requires runtime.v1');
  if (!isGptIntegrationConfigV1(context.config)) {
    throw new TypeError('GPT integration config is invalid');
  }
  const auction = exactCapability<ProductionAuctionCapability>(context.interfaces, 'auction.v1');
  const slotCapability = exactCapability<ProductionSlotsCapability>(context.interfaces, 'slots.v1');
  const aps = exactCapability<ProductionApsCapability>(context.interfaces, 'aps.v1');
  const render = exactCapability<ProductionRenderCapability>(context.interfaces, 'render.v1');
  const messages = exactCapability<ProductionMessagesCapability>(context.interfaces, 'messages.v1');
  const trace = exactCapability<ProductionTraceCapability>(context.interfaces, 'trace.v1');
  const document = runtime.document;
  if (
    !auction ||
    !slotCapability ||
    !render ||
    !messages ||
    !trace ||
    !document?.defaultView ||
    typeof auction.session?.replaceNavigation !== 'function' ||
    typeof runtime.protectFirstDisplayAttemptBatch !== 'function' ||
    typeof slotCapability.attachPhysicalService !== 'function' ||
    typeof render.attachPucGamAttemptRegistrar !== 'function' ||
    typeof render.bindArtifactRetirement !== 'function' ||
    typeof render.createAttempt !== 'function' ||
    typeof render.createSlotOperation !== 'function' ||
    typeof render.commitPageBids !== 'function' ||
    typeof render.mintLifecycleTicket !== 'function' ||
    typeof render.renderWinner !== 'function' ||
    (aps !== undefined && typeof aps.renderPuc !== 'function') ||
    typeof trace.observations?.publish !== 'function'
  ) {
    throw new TypeError('GPT capability graph is malformed');
  }

  const scope = new DisposableStack((error) => log.warn('GPT preparation disposal failed', error));
  context.onDispose(() => scope.dispose());
  let active = false;
  scope.onDispose(() => {
    active = false;
  });
  const googletag = createBrowserGoogletagAdapter(
    document.defaultView as unknown as Parameters<typeof createBrowserGoogletagAdapter>[0],
    {
      reportDiagnosticsFailure: (code) => log.warn('GPT diagnostics identity unavailable', code),
    }
  );
  scope.onDispose(() => googletag.dispose());
  const reconciliation = createBrowserSlotReconciliationBoundary(
    document,
    document.defaultView.MutationObserver
  );
  const slots = createSlotService({
    bindCommittedArtifactRetirement: render.bindArtifactRetirement,
    disposeCommittedArtifact: (navigationGeneration, registeredSlotId, expectedArtifact) => {
      const artifact = render.artifacts.current(registeredSlotId);
      if (artifact === expectedArtifact && artifact.navigationGeneration === navigationGeneration)
        render.artifacts.release(artifact);
    },
    googletag,
    ...(reconciliation ? { reconciliation } : {}),
  });
  scope.onDispose(() => slots.dispose());
  const targeting = createTargetingService();
  scope.onDispose(() => targeting.dispose());
  const startup = createGptStartup({ googletag, slots: () => slots });
  const diagnosticsEnabled = gptDiagnosticsActive(runtime);
  const diagnosticsFacts = diagnosticsEnabled
    ? createGptDiagnosticsFactBuffer({
        onOverflow: (droppedFacts) =>
          log.warn('GPT diagnostics fact buffer overflow', droppedFacts),
      })
    : undefined;
  if (diagnosticsFacts) scope.onDispose(diagnosticsFacts.dispose);
  let pucBridge: PucBridge | undefined;
  let takeoverReconciliationRelease: (() => void) | undefined;
  let laterLifecycleActive = false;
  let laterLifecycleRelease: (() => void) | undefined;
  const gptCapability: GptCapabilityV1 = Object.freeze({
    activateLaterLifecycle: () => {
      if (!active || laterLifecycleActive) {
        throw new TypeError('GPT later lifecycle is unavailable');
      }
      const currentBridge = pucBridge;
      if (!currentBridge) throw new TypeError('GPT later bridge is unavailable');
      const releaseReconciliation = takeoverReconciliationRelease;
      if (!releaseReconciliation) {
        throw new TypeError('GPT takeover reconciliation owner is unavailable');
      }
      takeoverReconciliationRelease = undefined;
      const controllers = new Set<AbortController>();
      let ownerActive = true;
      laterLifecycleActive = true;
      const rejected = (navigation?: NavigationSession): GptLaterNavigationResult => {
        const rejectedNavigation =
          navigation ?? auction.session.currentNavigation ?? auction.navigation;
        return Object.freeze({
          status: 'rejected',
          navigationGeneration: rejectedNavigation.generation,
          current: ownerActive && active && rejectedNavigation.isCurrent(),
        });
      };
      const release = (): void => {
        if (!ownerActive) return;
        ownerActive = false;
        laterLifecycleActive = false;
        if (laterLifecycleRelease === release) laterLifecycleRelease = undefined;
        for (const controller of controllers) controller.abort();
        controllers.clear();
        releaseReconciliation();
      };
      laterLifecycleRelease = release;
      return Object.freeze({
        navigate: async (path: string): Promise<GptLaterNavigationResult> => {
          if (
            !ownerActive ||
            !active ||
            typeof path !== 'string' ||
            path.length === 0 ||
            path.length > 4_096 ||
            !path.startsWith('/')
          ) {
            return rejected();
          }
          const replacement = auction.session.replaceNavigation();
          if (!replacement.ok || !ownerActive || !active) return rejected();
          const navigation = replacement.value;
          const controller = new AbortController();
          controllers.add(controller);
          const abortForNavigation = (): void => controller.abort();
          navigation.signal.addEventListener('abort', abortForNavigation, { once: true });
          try {
            const fetcher = globalThis.fetch;
            if (typeof fetcher !== 'function') return rejected(navigation);
            const response = await fetcher(`/_ts/page-bids?path=${encodeURIComponent(path)}`, {
              credentials: 'include',
              headers: { 'X-TSJS-Page-Bids': '1' },
              signal: controller.signal,
            });
            if (!ownerActive || !active || !navigation.isCurrent() || !response.ok) {
              return rejected(navigation);
            }
            const candidate = await response.json();
            if (!ownerActive || !active || !navigation.isCurrent()) return rejected(navigation);
            if (
              !render.commitPageBids(navigation, slots.projectionRegistry(navigation), candidate)
            ) {
              return rejected(navigation);
            }
            const projection = navigation.currentAuctionProjection as
              Readonly<BrowserAuctionProjectionV1> | undefined;
            if (!projection || !ownerActive || !active || !navigation.isCurrent()) {
              return rejected(navigation);
            }
            await publishInitialGptProjection(document, {
              googletag,
              navigation,
              projection,
              protect: () => true,
              pucBridge: currentBridge,
              render,
              requestClass: 'page-bids',
              slots,
              targeting,
            });
            if (!ownerActive || !active || !navigation.isCurrent()) return rejected(navigation);
            return Object.freeze({
              status: 'committed',
              navigationGeneration: navigation.generation,
              current: true,
            });
          } catch (error) {
            if (!controller.signal.aborted && ownerActive && navigation.isCurrent()) {
              log.warn('GPT page-bids navigation failed', error);
            }
            return rejected(navigation);
          } finally {
            navigation.signal.removeEventListener('abort', abortForNavigation);
            controllers.delete(controller);
          }
        },
        release,
      });
    },
    adapter: googletag,
    directAuctionUnitForSlot: (slot: object): Readonly<object> | undefined => {
      const navigation = auction.session.currentNavigation;
      if (!active || !navigation?.isCurrent()) return undefined;
      const records = slots.snapshotRegisteredSlots(navigation) ?? Object.freeze([]);
      for (let index = 0; index < records.length; index += 1) {
        const record = records[index];
        if (
          record?.directAuctionUnit &&
          slots.isBoundGptSlot(navigation.generation, record.registeredSlotId, slot)
        ) {
          return record.directAuctionUnit;
        }
      }
      return undefined;
    },
    installRefreshPolicy: startup.installRefreshPolicy,
    navigation: () => {
      const navigation = auction.session.currentNavigation;
      return active && navigation?.isCurrent() ? navigation : undefined;
    },
    slots,
  });
  const eventsCapability = Object.freeze({
    subscribe: (listener: (fact: Readonly<Record<string, unknown>>) => void): (() => void) => {
      const release =
        active && diagnosticsFacts
          ? diagnosticsFacts.activate(listener as Parameters<typeof diagnosticsFacts.activate>[0])
          : undefined;
      if (!release) {
        throw new TypeError('GPT event subscription is unavailable');
      }
      return release;
    },
  });
  const cacheCapability = Object.freeze({});

  return Object.freeze({
    activate: (activation: IntegrationActivationContext) => {
      const { afterCommit, onDispose } = activation;
      const adoptionCandidate = activation.adoption;
      if (active) throw new Error('GPT already activated');
      if (adoptionCandidate !== undefined) {
        const selected = persistentFirstDisplaySliceSelectedV1(adoptionCandidate, 'gpt_initial');
        const initialState = selected
          ? snapshotPersistentFirstDisplaySliceStateV1(adoptionCandidate, 'gpt_initial')
          : undefined;
        if (
          selected === undefined ||
          (selected &&
            (!initialState ||
              initialState.values.length !== 1 ||
              initialState.values[0]?.[0] !== 'protocol_version' ||
              initialState.values[0][1] !== 1))
        ) {
          throw new TypeError('GPT first-display parser state is invalid');
        }
      }
      const diagnosticsEventRelease: { current?: () => void } = {};
      const diagnosticsRelease: { current?: () => void } = {};
      const pbsCacheBridgeRelease: { current?: () => void } = {};
      const publisherRelease: { current?: () => void } = {};
      const slotServiceRelease: { current?: () => void } = {};
      const bridgeRelease: { current?: () => void } = {};
      const pucRegistrarRelease: { current?: () => void } = {};
      onDispose(resetGuardState);
      onDispose(() => diagnosticsEventRelease.current?.());
      onDispose(() => diagnosticsRelease.current?.());
      onDispose(() => pbsCacheBridgeRelease.current?.());
      onDispose(() => publisherRelease.current?.());
      onDispose(() => slotServiceRelease.current?.());
      onDispose(() => bridgeRelease.current?.());
      onDispose(() => pucRegistrarRelease.current?.());
      onDispose(() => {
        active = false;
        const release = laterLifecycleRelease;
        laterLifecycleRelease = undefined;
        release?.();
        const releaseTakeoverReconciliation = takeoverReconciliationRelease;
        takeoverReconciliationRelease = undefined;
        releaseTakeoverReconciliation?.();
      });

      slotServiceRelease.current = slotCapability.attachPhysicalService(slots);
      const diagnosticsAdoption =
        adoptionCandidate === undefined
          ? undefined
          : adoptInitialGptDiagnosticsFromHandoff(adoptionCandidate, googletag);
      const adoption =
        adoptionCandidate === undefined
          ? undefined
          : adoptInitialGptSlotsFromHandoff(
              adoptionCandidate,
              auction.navigation.generation,
              slots,
              render.artifacts,
              targeting,
              googletag
            );
      if (adoptionCandidate !== undefined && (!diagnosticsAdoption || !adoption)) {
        throw new TypeError('GPT first-display adoption is invalid');
      }
      const factAdoption =
        adoptionCandidate === undefined
          ? undefined
          : adoptInitialGptFactsFromHandoff(adoptionCandidate, diagnosticsFacts, googletag);
      if (adoptionCandidate !== undefined && !factAdoption) {
        throw new TypeError('GPT first-display diagnostics facts are invalid');
      }
      slots.start();
      takeoverReconciliationRelease = slots.activateReconciliation();
      const bridge = createPucBridge({
        messaging: messages.messaging,
        mintLifecycleTicket: render.mintLifecycleTicket,
        ...(aps ? { mountAps: aps.renderPuc } : {}),
        now: () => document.defaultView!.performance.now(),
        reservations: render.reservations,
        resizeCollapsedShell: resizeCollapsedPucShell,
        slots,
      });
      const ticketAdoption =
        adoptionCandidate === undefined
          ? undefined
          : adoptInitialPucTicketsFromHandoff(adoptionCandidate, bridge);
      if (adoptionCandidate !== undefined && !ticketAdoption) {
        throw new TypeError('GPT first-display ticket adoption is invalid');
      }
      pucBridge = bridge;
      bridgeRelease.current = () => {
        bridge.dispose();
        if (pucBridge === bridge) pucBridge = undefined;
      };
      pucRegistrarRelease.current = render.attachPucGamAttemptRegistrar((input) =>
        bridge.registerGamAttempt(input)
      );
      const releaseDiagnostics = googletag.observeDiagnostics((fact) => {
        const observation = projectGptTraceFact(fact);
        if (observation) trace.observations.publish(observation);
        diagnosticsFacts?.publish(fact);
      });
      if (!releaseDiagnostics) throw new Error('GPT diagnostics event boundary is unavailable');
      diagnosticsRelease.current = releaseDiagnostics;
      if (diagnosticsFacts) {
        const releaseDiagnosticEvents = activateGptDiagnosticsEventListeners(googletag);
        if (!releaseDiagnosticEvents) {
          throw new Error('GPT diagnostics-only event listeners are unavailable');
        }
        diagnosticsEventRelease.current = releaseDiagnosticEvents;
      }
      pbsCacheBridgeRelease.current = installPbsCacheBridge(
        document,
        auction,
        () => active,
        trace.observations.publish
      );
      publisherRelease.current = startup.activate();
      installGptGuard();
      active = true;
      afterCommit(() => {
        startup.start(context.config);
        if (adoption) return;
        const currentBridge = pucBridge;
        if (!active || currentBridge !== bridge) return;
        void publishInitialGptProjection(document, {
          googletag,
          navigation: auction.navigation,
          projection: auction.projection,
          protect: runtime.protectFirstDisplayAttemptBatch,
          pucBridge: currentBridge,
          render,
          slots,
          targeting,
        }).catch((error) => {
          if (auction.navigation.isCurrent()) log.warn('GPT initial projection failed', error);
        });
      });
    },
    interfaces: Object.freeze({
      'gpt.v1': gptCapability,
      'gpt.events.v1': eventsCapability,
      'pbs_cache.baseline.v1': cacheCapability,
    }),
  });
}

/** Build the release-bound GPT module registered by the coordinated runtime. */
export function createGptIntegrationRegistration(releaseId: string): IntegrationRegistration {
  const prepare = (context: IntegrationPrepareContext) => prepareProductionGpt(context);
  return Object.freeze({
    abi: 1,
    id: GPT_INTEGRATION_ID,
    phase: 'takeover',
    releaseId,
    prepareSync: prepare,
    prepare,
  });
}
