import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';
import type { GoogletagAdapter, GoogletagFacade } from '../../adapters/googletag';
import {
  isAuctionCandidateIdV1,
  isRendererReservationIdV1,
} from '../../core/contracts/auction_projection';
import type { BrowserAuctionBidV1, BrowserAuctionProjectionV1 } from '../../core/types';
import type { NavigationSession } from '../../kernel/sessions';
import {
  createSlotOperation,
  type CommittedRenderArtifact,
  type RenderAttempt,
  type RenderFailureReason,
  type SlotOperationCreationResult,
  type SlotOperationOptions,
} from '../../services/render';
import type { PucBridge, PucGamAttemptInput } from '../../services/puc_bridge';
import type { ReservationService } from '../../services/reservations';
import type { SlotRequestOutcome, SlotService } from '../../services/slots';
import type {
  TargetingBoundary,
  TargetingOwnership,
  TargetingService,
} from '../../services/targeting';

import { installGptGuard, resetGuardState } from './script_guard';

export const GPT_INTEGRATION_ID = 'gpt' as const;

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

interface GptIntegrationRuntime {
  readonly start: (config: unknown) => void;
}

export interface GptSlotOperationInput extends Omit<PucGamAttemptInput, 'attempt'> {
  readonly attempt: RenderAttempt;
  readonly createFallback?: SlotOperationOptions['createFallback'];
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
    if (
      !projection ||
      !objectIsFrozenIntrinsic(projection) ||
      !objectIsFrozenIntrinsic(bid) ||
      !objectIsFrozenIntrinsic(bid.renderSource) ||
      !objectIsFrozenIntrinsic(bid.targeting) ||
      !isAuctionCandidateIdV1(bid.candidateId) ||
      !isRendererReservationIdV1(bid.rendererReservationId) ||
      bid.slot !== input.attempt.slot ||
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
  bid: BrowserAuctionBidV1
): readonly (readonly [string, string])[] | undefined {
  try {
    const unsortedNames = objectGetOwnPropertyNamesIntrinsic(bid.targeting);
    const names: string[] = [];
    for (let index = 0; index < unsortedNames.length; index += 1) {
      const name = unsortedNames[index];
      if (name === undefined) return undefined;
      let insertion = names.length;
      while (insertion > 0 && (names[insertion - 1] as string) > name) insertion -= 1;
      for (let move = names.length; move > insertion; move -= 1) {
        names[move] = names[move - 1] as string;
      }
      names[insertion] = name;
    }
    if (names.length > 32 || objectGetOwnPropertySymbolsIntrinsic(bid.targeting).length !== 0) {
      return undefined;
    }
    const entries: Array<readonly [string, string]> = [
      Object.freeze(['hb_adid', bid.rendererReservationId]),
    ];
    for (let index = 0; index < names.length; index += 1) {
      const key = names[index];
      if (!key || key === 'hb_adid') return undefined;
      const descriptor = objectGetOwnPropertyDescriptorIntrinsic(bid.targeting, key);
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
  const entries = targetingEntries(input.bid);
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
  const operation = createSlotOperation({
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

function readGptRuntime(
  interfaces: Readonly<Record<string, unknown>>
): GptIntegrationRuntime | undefined {
  const descriptor = Object.getOwnPropertyDescriptor(interfaces, GPT_INTEGRATION_ID);
  if (!descriptor || !('value' in descriptor)) return undefined;
  const candidate = descriptor.value;
  if (
    typeof candidate !== 'object' ||
    candidate === null ||
    Array.isArray(candidate) ||
    !Object.isFrozen(candidate) ||
    Reflect.ownKeys(candidate).length !== 1
  ) {
    return undefined;
  }
  const start = Object.getOwnPropertyDescriptor(candidate, 'start');
  if (!start || !('value' in start) || typeof start.value !== 'function') return undefined;
  return candidate as GptIntegrationRuntime;
}

/** Build the release-bound GPT module registered by the coordinated runtime. */
export function createGptIntegrationRegistration(release: string): IntegrationRegistration {
  return Object.freeze({
    id: GPT_INTEGRATION_ID,
    release,
    prepare: ({ config, interfaces }: IntegrationPrepareContext) => {
      if (!validFrozenConfig(config)) throw new TypeError('GPT integration config is invalid');
      const runtime = readGptRuntime(interfaces);
      if (!runtime) throw new TypeError('GPT integration runtime is unavailable');

      return Object.freeze({
        activate: ({ afterCommit, onDispose }: IntegrationActivationContext) => {
          // Register restoration before the first live browser mutation.
          onDispose(resetGuardState);
          installGptGuard();
          afterCommit(() => runtime.start(config));
        },
      });
    },
  });
}
