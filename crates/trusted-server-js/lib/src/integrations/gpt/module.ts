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
  BrowserAuctionBidV1,
  BrowserAuctionProjectionV1,
  BrowserAuctionSlotV1,
  CacheFetchPolicyV1,
} from '../../core/types';
import { log } from '../../core/log';
import { DisposableStack } from '../../kernel/disposable';
import type { RuntimeCapabilityV1 } from '../../kernel/runtime';
import type { NavigationSession, RenderAttemptScope, RuntimeSession } from '../../kernel/sessions';
import type { IdentityGenerationResult } from '../../kernel/identity';
import type {
  CacheAdmResolutionOptions,
  CommittedRenderArtifact,
  DirectAdmIframeConstructor,
  DirectCacheAttemptOptions,
  RenderAttempt,
  RenderAttemptCreationResult,
  RenderFailureReason,
  RendererNonceRegistry,
  SlotOperationCreationResult,
  SlotOperationOptions,
} from '../../services/render';
import { resizeCollapsedPucShell } from '../../core/puc_shell';
import {
  createPucBridge,
  type PucBridge,
  type PucGamAttemptInput,
} from '../../services/puc_bridge';
import type { ReservationService } from '../../services/reservations';
import {
  createBrowserSlotReconciliationBoundary,
  createSlotService,
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

const MAX_CONFIG_DEPTH = 16;
const MAX_CONFIG_NODES = 512;
const MAX_CONFIG_MEMBERS = 256;
const arrayIsArrayIntrinsic = Array.isArray;
const numberIsFiniteIntrinsic = Number.isFinite;
const objectGetOwnPropertyDescriptorIntrinsic = Object.getOwnPropertyDescriptor;
const objectGetOwnPropertyNamesIntrinsic = Object.getOwnPropertyNames;
const objectGetOwnPropertySymbolsIntrinsic = Object.getOwnPropertySymbols;
const objectGetPrototypeOfIntrinsic = Object.getPrototypeOf;
const objectIsFrozenIntrinsic = Object.isFrozen;
const promiseThenIntrinsic = Promise.prototype.then;
const reflectApplyIntrinsic = Reflect.apply;

interface ProductionAuctionCapability {
  readonly navigation: NavigationSession;
  readonly projection: Readonly<BrowserAuctionProjectionV1>;
  readonly session: RuntimeSession;
}

interface ProductionSlotsCapability {
  readonly attachPhysicalService: (service: SlotService) => () => void;
}

interface ProductionRenderCapability {
  readonly attachPucGamAttemptRegistrar: (
    registrar: (input: PucGamAttemptInput) => boolean
  ) => () => void;
  readonly artifacts: Readonly<{
    current: (slot: string) => CommittedRenderArtifact | undefined;
    release: (artifact: CommittedRenderArtifact) => boolean;
  }>;
  readonly cachePolicy?: Readonly<CacheFetchPolicyV1>;
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
  readonly publisherOrigin: string;
  readonly prepareAdmIframe: DirectAdmIframeConstructor;
  readonly registerRenderer: (
    type: 'cache',
    renderer: (attempt: RenderAttempt, container: HTMLElement) => boolean
  ) => () => void;
  readonly rendererNonces: RendererNonceRegistry;
  readonly renderDirectCacheAttempt: (options: DirectCacheAttemptOptions) => boolean;
  readonly renderWinner: (attempt: RenderAttempt) => boolean;
  readonly reservations: ReservationService;
  readonly resolveCacheAdmAttempt: (options: CacheAdmResolutionOptions) => boolean;
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
  readonly bid: BrowserAuctionBidV1;
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
        if (!name || name === 'hb_adid') return false;
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
    const entries: Array<readonly [string, string]> = [
      Object.freeze(['hb_adid', bid.rendererReservationId]),
    ];
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

function terminalLatch(attempt: RenderAttempt): Promise<unknown> {
  return new Promise((resolve) => {
    if (!attempt.onSettled(resolve)) resolve(attempt.snapshot().outcome);
  });
}

interface PreparedInitialGptWinner {
  readonly attempt: RenderAttempt;
  readonly bid: BrowserAuctionBidV1;
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
  let winnerIndex = 0;
  for (let index = 0; index < projection.auction.results.length; index += 1) {
    const decision = projection.auction.results[index];
    const placement = projection.slots[index];
    if (!decision || !placement || decision.outcome !== 'winner') continue;
    const bid = projection.bids[winnerIndex];
    winnerIndex += 1;
    if (!bid || !navigation.isCurrent()) continue;
    const owner = batch.createRenderAttempt(decision.slot);
    if (!owner.ok) continue;
    const created = render.createAttempt(owner.value);
    if (!created.ok) continue;
    prepared.push(
      Object.freeze({
        attempt: created.value,
        bid,
        binding: physicalBySlot.get(decision.slot),
        decision,
        owner: owner.value,
        placement,
        terminal: terminalLatch(created.value),
      })
    );
  }
  if (prepared.length === 0) return;
  input.protect(Object.freeze(prepared.map(({ terminal }) => terminal)));

  await Promise.all(
    prepared.map(async ({ attempt, bid, binding, decision, owner, placement }) => {
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
    })
  );
}

function validFrozenConfig(candidate: unknown): boolean {
  const seen = new Set<object>();
  let nodes = 0;
  const visit = (value: unknown, depth: number, topLevel: boolean): boolean => {
    if (value === undefined) return topLevel;
    if (value === null || typeof value === 'string' || typeof value === 'boolean') return true;
    if (typeof value === 'number') return numberIsFiniteIntrinsic(value);
    if (typeof value !== 'object' || depth > MAX_CONFIG_DEPTH || nodes >= MAX_CONFIG_NODES) {
      return false;
    }
    if (seen.has(value) || !objectIsFrozenIntrinsic(value)) return false;
    seen.add(value);
    nodes += 1;

    const array = arrayIsArrayIntrinsic(value);
    const prototype = objectGetPrototypeOfIntrinsic(value);
    if (
      (!array && prototype !== Object.prototype && prototype !== null) ||
      (array && prototype !== Array.prototype)
    ) {
      return false;
    }
    if (objectGetOwnPropertySymbolsIntrinsic(value).length !== 0) return false;
    const names = objectGetOwnPropertyNamesIntrinsic(value);
    if (names.length > MAX_CONFIG_MEMBERS + (array ? 1 : 0)) return false;
    if (array) {
      const length = objectGetOwnPropertyDescriptorIntrinsic(value, 'length');
      if (!length || !('value' in length) || names.length !== length.value + 1) return false;
      for (let index = 0; index < length.value; index += 1) {
        const descriptor = objectGetOwnPropertyDescriptorIntrinsic(value, String(index));
        if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return false;
        if (!visit(descriptor.value, depth + 1, false)) return false;
      }
      return true;
    }

    for (const name of names) {
      const descriptor = objectGetOwnPropertyDescriptorIntrinsic(value, name);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return false;
      if (!visit(descriptor.value, depth + 1, false)) return false;
    }
    return true;
  };

  try {
    return visit(candidate, 0, true);
  } catch {
    return false;
  }
}

function prepareProductionGpt(context: IntegrationPrepareContext): PreparedIntegration {
  const runtime = exactCapability<RuntimeCapabilityV1>(context.interfaces, 'runtime.v1');
  if (!runtime) throw new TypeError('GPT requires runtime.v1');
  if (!validFrozenConfig(context.config)) throw new TypeError('GPT integration config is invalid');
  const auction = exactCapability<ProductionAuctionCapability>(context.interfaces, 'auction.v1');
  const slotCapability = exactCapability<ProductionSlotsCapability>(context.interfaces, 'slots.v1');
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
    typeof render.createAttempt !== 'function' ||
    typeof render.createSlotOperation !== 'function' ||
    typeof render.commitPageBids !== 'function' ||
    typeof render.mintLifecycleTicket !== 'function' ||
    typeof render.prepareAdmIframe !== 'function' ||
    typeof render.registerRenderer !== 'function' ||
    typeof render.renderDirectCacheAttempt !== 'function' ||
    typeof render.renderWinner !== 'function' ||
    typeof render.resolveCacheAdmAttempt !== 'function' ||
    typeof render.publisherOrigin !== 'string' ||
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
    disposeCommittedArtifact: (navigationGeneration, registeredSlotId) => {
      const artifact = render.artifacts.current(registeredSlotId);
      if (artifact?.navigationGeneration === navigationGeneration)
        render.artifacts.release(artifact);
    },
    googletag,
    ...(reconciliation ? { reconciliation } : {}),
  });
  scope.onDispose(() => slots.dispose());
  const targeting = createTargetingService();
  scope.onDispose(() => targeting.dispose());
  const fetchCache = globalThis.fetch;
  const cacheRenderer = (attempt: RenderAttempt, container: HTMLElement): boolean => {
    if (!active) return false;
    const cachePolicy = render.cachePolicy;
    if (!cachePolicy) {
      attempt.fail('descriptor_invalid');
      return false;
    }
    if (typeof fetchCache !== 'function') {
      attempt.fail('cache_network_error');
      return false;
    }
    return render.renderDirectCacheAttempt({
      attempt,
      cachePolicy,
      container,
      fetcher: (input, init) => fetchCache(input, init),
      prepareIframe: render.prepareAdmIframe,
      publisherOrigin: render.publisherOrigin,
    });
  };
  const resolveCacheAdm = (
    attempt: Parameters<NonNullable<Parameters<typeof createPucBridge>[0]['resolveCacheAdm']>>[0],
    onResolved: Parameters<NonNullable<Parameters<typeof createPucBridge>[0]['resolveCacheAdm']>>[1]
  ): boolean => {
    const cachePolicy = render.cachePolicy;
    if (!cachePolicy) {
      attempt.fail('descriptor_invalid');
      return false;
    }
    if (typeof fetchCache !== 'function') {
      attempt.fail('cache_network_error');
      return false;
    }
    return render.resolveCacheAdmAttempt({
      attempt: attempt as RenderAttempt,
      cachePolicy,
      fetcher: (input, init) => fetchCache(input, init),
      onResolved,
    });
  };
  const rendererUrl = new URL('/integrations/aps/renderer/v1', render.publisherOrigin).href;
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
  let criticalReconciliationRelease: (() => void) | undefined;
  let laterLifecycleActive = false;
  let laterLifecycleRelease: (() => void) | undefined;
  const gptCapability: GptCapabilityV1 = Object.freeze({
    activateLaterLifecycle: () => {
      if (!active || laterLifecycleActive) {
        throw new TypeError('GPT later lifecycle is unavailable');
      }
      const currentBridge = pucBridge;
      if (!currentBridge) throw new TypeError('GPT later bridge is unavailable');
      const releaseReconciliation = criticalReconciliationRelease;
      if (!releaseReconciliation) {
        throw new TypeError('GPT critical reconciliation owner is unavailable');
      }
      criticalReconciliationRelease = undefined;
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
              !render.commitPageBids(
                navigation,
                slots.projectionRegistry(navigation),
                candidate
              )
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
  const cacheCapability = Object.freeze({ render: cacheRenderer });

  return Object.freeze({
    activate: ({ afterCommit, onDispose }: IntegrationActivationContext) => {
      if (active) throw new Error('GPT already activated');
      const cacheRelease: { current?: () => void } = {};
      const diagnosticsEventRelease: { current?: () => void } = {};
      const diagnosticsRelease: { current?: () => void } = {};
      const publisherRelease: { current?: () => void } = {};
      const slotServiceRelease: { current?: () => void } = {};
      const bridgeRelease: { current?: () => void } = {};
      const pucRegistrarRelease: { current?: () => void } = {};
      onDispose(resetGuardState);
      onDispose(() => cacheRelease.current?.());
      onDispose(() => diagnosticsEventRelease.current?.());
      onDispose(() => diagnosticsRelease.current?.());
      onDispose(() => publisherRelease.current?.());
      onDispose(() => slotServiceRelease.current?.());
      onDispose(() => bridgeRelease.current?.());
      onDispose(() => pucRegistrarRelease.current?.());
      onDispose(() => {
        active = false;
        const release = laterLifecycleRelease;
        laterLifecycleRelease = undefined;
        release?.();
        const releaseCriticalReconciliation = criticalReconciliationRelease;
        criticalReconciliationRelease = undefined;
        releaseCriticalReconciliation?.();
      });

      slotServiceRelease.current = slotCapability.attachPhysicalService(slots);
      slots.start();
      criticalReconciliationRelease = slots.activateReconciliation();
      const bridge = createPucBridge({
        messaging: messages.messaging,
        mintLifecycleTicket: render.mintLifecycleTicket,
        publisherOrigin: render.publisherOrigin,
        rendererNonces: render.rendererNonces,
        rendererUrl,
        reservations: render.reservations,
        resolveCacheAdm,
        resizeCollapsedShell: resizeCollapsedPucShell,
      });
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
      publisherRelease.current = startup.activate();
      cacheRelease.current = render.registerRenderer('cache', cacheRenderer);
      installGptGuard();
      active = true;
      afterCommit(() => {
        startup.start(context.config);
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
  return Object.freeze({
    abi: 1,
    id: GPT_INTEGRATION_ID,
    phase: 'critical',
    releaseId,
    prepare: (context: IntegrationPrepareContext) => prepareProductionGpt(context),
  });
}
