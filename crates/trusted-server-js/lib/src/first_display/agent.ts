import type { BootFailureReason } from '../kernel/fallback';
import {
  createNavigationIdentityIssuerFromSource,
  type NavigationIdentityIssuer,
} from '../kernel/identity';
import type { FirstDisplaySliceId } from '../kernel/release_catalog';
import type { FirstDisplayGptDiagnosticsV1 } from '../shared/takeover';
import {
  captureMutationObservedBindings,
  createFirstDisplayParserStateCollector,
} from '../shared/first_display_registration';
import type { FirstDisplaySliceActivationContext } from '../shared/first_display_transaction';
import {
  createFirstDisplayHandoffOwner,
  type FinalizedFirstDisplayHandoffV1,
  type FirstDisplayHandoffOwner,
} from '../shared/first_display_handoff';

import type {
  FirstDisplayGoogletagBatchInput,
  FirstDisplayGptBoundCycleV1,
  FirstDisplayGptDiagnosticCycleV1,
} from './adapters/googletag';
import { createFirstDisplayAdmRenderBridge } from './adm_render_bridge';
import { createFirstDisplayProjectedDriver, type FirstDisplayRenderBridgeV1 } from './driver';
import type { FirstDisplayApsProtocolV1 } from './leaf/aps_protocol';
import type { FirstDisplayGptProtocolV1 } from './leaf/gpt_protocol';
import { registerCurrentFirstDisplayComponent } from './registration_client';
import type {
  FirstDisplaySliceHost,
  InitialSliceInstaller,
  OptionalFirstDisplaySliceId,
} from './slices/definition';
import {
  acceptServerFirstDisplayBatchV1,
  type FirstDisplayAuctionProtocolId,
  type FirstDisplayBatchOutcomeV1,
  type FirstDisplayBatchV1,
} from './leaf/projection';
import type { FirstDisplayRenderBridgeOptionsV1 } from './render_bridge';

const MAX_U32 = 4_294_967_295;
const AUCTION_PROTOCOLS = ['aps', 'gpt', 'prebid'] as const;
const ACTION_KINDS = new Set(['gpt_adm', 'aps']);
const TERMINAL_RESULTS = new Set(['accepted', 'failed', 'cancelled']);

export type {
  FirstDisplayAuctionProtocolId,
  FirstDisplayBatchOutcomeV1,
  FirstDisplayBatchV1,
  FirstDisplayProjectedKind,
} from './leaf/projection';
export type FirstDisplayTerminalResult = 'accepted' | 'failed' | 'cancelled';
export type FirstDisplayAgentState =
  'ready' | 'active' | 'terminal' | 'painted' | 'failed' | 'disposed';

export interface FirstDisplayBootstrapController {
  readonly state: 'installing' | 'agent_registered' | 'action_started' | 'settled' | 'failed';
  readonly startedAtMs: number;
  readonly registerAgent: () => boolean;
  readonly startAction: () => boolean;
  readonly settle: () => boolean;
  readonly fail: (reason: BootFailureReason) => boolean;
}

export interface FirstDisplayDriver {
  readonly start: (
    outcomes: readonly FirstDisplayBatchOutcomeV1[],
    onFirstAction: () => boolean,
    onTerminal: (slotId: string, result: FirstDisplayTerminalResult, reason: string | null) => void
  ) => void;
  readonly sealTsAdmission: () => void;
  readonly closeIngress: () => boolean;
  readonly captureHandoff: () => FirstDisplayDriverHandoffV1 | undefined;
  readonly detachCommittedArtifacts: () => boolean;
  readonly dispose: () => void;
}

export interface FirstDisplayDriverHandoffV1 {
  readonly artifacts: readonly Readonly<{
    identity: object;
    kind: 'gpt_adm' | 'aps';
    owner: 'trusted_server' | 'publisher';
    slotId: string;
    token: string;
  }>[];
  readonly cycles: readonly FirstDisplayGptBoundCycleV1[];
  readonly diagnosticCycles: readonly Readonly<FirstDisplayGptDiagnosticCycleV1>[];
  readonly clockEpochMs: number;
  readonly gptDiagnostics: Readonly<FirstDisplayGptDiagnosticsV1>;
  readonly identities: readonly object[];
  readonly nextReservationOrdinal: number;
  readonly nextTraceTokenOrdinal: number;
  readonly nextTicketOrdinal: number;
  readonly tombstones: readonly Readonly<{
    kind: 'reservation' | 'ticket';
    value: string;
    expiresAtMs: number;
    ordinal: number;
  }>[];
}

export interface FirstDisplayPaintScheduler {
  readonly hidden: () => boolean;
  readonly requestFrame: (callback: () => void) => void;
  readonly scheduleHidden: (callback: () => void) => void;
}

export interface FirstDisplayAgentOptions {
  readonly batch: unknown;
  readonly bootstrap: FirstDisplayBootstrapController;
  readonly driver: FirstDisplayDriver;
  readonly performance: Readonly<{
    mark: (name: string) => void;
    measure?: (name: string, startMark: string, endMark: string) => void;
  }>;
  readonly paint: FirstDisplayPaintScheduler;
  readonly onProtectedPaint: () => void;
  readonly onFailure: (reason: BootFailureReason) => void;
  readonly onPrebidAdmissionFailure?: (reason: 'prebid_admission_failed') => void;
  readonly mutationDocument?: Document;
  readonly initialMutationRevision?: number;
  readonly now?: () => number;
  readonly identityIssuer?: NavigationIdentityIssuer;
  readonly parserState?: () => readonly Readonly<{
    sliceId: string;
    observations: readonly string[];
    values: readonly (readonly [string, string | number | boolean | null])[];
  }>[];
  readonly handoff?: Readonly<{
    releaseId: string;
    generation: number;
    slices: readonly FirstDisplaySliceId[];
  }>;
}

/** Bootstrap-owned dependencies supplied only to the release-bound base component. */
export interface FirstDisplayAgentRegistrationHostV1 {
  readonly options: Omit<FirstDisplayAgentOptions, 'driver'> &
    Readonly<{
      gptInput: Omit<FirstDisplayGoogletagBatchInput, 'onNativeMutation' | 'projection'>;
      onAgentReady?: (agent: FirstDisplayAgent) => void;
    }>;
  readonly sliceBindings: (id: string) => unknown;
}

function createBrowserMessageChannel(
  browser: Window
): ReturnType<FirstDisplayRenderBridgeOptionsV1['createChannel']> {
  const constructor = Reflect.get(browser, 'MessageChannel');
  if (typeof constructor !== 'function') {
    throw new TypeError('tsjs');
  }
  const channel = Reflect.construct(constructor, []) as Record<PropertyKey, unknown>;
  const port1 = Reflect.get(channel, 'port1');
  const port2 = Reflect.get(channel, 'port2');
  if (typeof port1 !== 'object' || port1 === null || typeof port2 !== 'object' || port2 === null) {
    throw new TypeError('tsjs');
  }
  return { port1, port2 } as ReturnType<FirstDisplayRenderBridgeOptionsV1['createChannel']>;
}

function fillBrowserRandom(browser: Window, bytes: Uint8Array): void {
  const crypto = Reflect.get(browser, 'crypto');
  const getRandomValues =
    typeof crypto === 'object' && crypto !== null
      ? Reflect.get(crypto, 'getRandomValues')
      : undefined;
  if (typeof getRandomValues !== 'function') throw new TypeError('tsjs');
  Reflect.apply(getRandomValues, crypto, [bytes]);
}

function readBrowserNow(browser: Window): number {
  const performance = Reflect.get(browser, 'performance');
  const now =
    typeof performance === 'object' && performance !== null
      ? Reflect.get(performance, 'now')
      : undefined;
  if (typeof now !== 'function') throw new TypeError('tsjs');
  return Reflect.apply(now, performance, []) as number;
}

interface PreparedFirstDisplayBaseV1 {
  readonly activate: (context: FirstDisplaySliceActivationContext) => void;
  readonly sliceHost: FirstDisplaySliceHost;
}

export interface FirstDisplayAgentSnapshotV1 {
  readonly state: FirstDisplayAgentState;
  readonly mutationRevision: number;
  readonly initialDisplayCommitted: boolean;
  readonly outcomes: readonly Readonly<{
    slotId: string;
    result: FirstDisplayTerminalResult | 'no_bid' | 'failed' | 'cancelled';
  }>[];
}

export interface FirstDisplayAgent {
  readonly state: FirstDisplayAgentState;
  readonly coversProtocols: (
    protocols: ReadonlyMap<FirstDisplayAuctionProtocolId, unknown>
  ) => boolean;
  readonly start: () => boolean;
  readonly admitTsBid: (complete: () => void) => boolean;
  readonly observeNativeMutation: () => boolean;
  readonly snapshot: () => FirstDisplayAgentSnapshotV1;
  readonly finalizeHandoff: () => FinalizedFirstDisplayHandoffV1 | undefined;
  readonly detachCommittedArtifacts: () => boolean;
  readonly dispose: () => void;
}

function protocolIdentity(candidate: unknown, expected: FirstDisplayAuctionProtocolId): boolean {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      !Object.isFrozen(candidate) ||
      Reflect.ownKeys(candidate).length !== 2
    ) {
      return false;
    }
    const version = Object.getOwnPropertyDescriptor(candidate, 'version');
    const id = Object.getOwnPropertyDescriptor(candidate, 'id');
    return Boolean(
      version?.enumerable &&
      'value' in version &&
      version.value === 1 &&
      id?.enumerable &&
      'value' in id &&
      id.value === expected
    );
  } catch {
    return false;
  }
}

function fullProtocolIdentity(
  candidate: unknown,
  expected: FirstDisplayAuctionProtocolId
): boolean {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      !Object.isFrozen(candidate)
    ) {
      return false;
    }
    const version = Object.getOwnPropertyDescriptor(candidate, 'version');
    const id = Object.getOwnPropertyDescriptor(candidate, 'id');
    return Boolean(
      version?.enumerable &&
      'value' in version &&
      version.value === 1 &&
      id?.enumerable &&
      'value' in id &&
      id.value === expected
    );
  } catch {
    return false;
  }
}

function captureProtocolRegistration(
  candidate: unknown,
  protocolId: FirstDisplayAuctionProtocolId,
  protocols: Map<FirstDisplayAuctionProtocolId, unknown>
): unknown {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      !Object.isFrozen(candidate)
    ) {
      return candidate;
    }
    const descriptors = Object.getOwnPropertyDescriptors(candidate);
    const register = descriptors.register;
    if (!register?.enumerable || !('value' in register) || typeof register.value !== 'function') {
      return candidate;
    }
    const original = register.value as (protocol: unknown) => unknown;
    const captured = Object.create(Object.prototype) as Record<PropertyKey, unknown>;
    const wrappedRegister = (protocol: unknown): (() => void) => {
      if (!fullProtocolIdentity(protocol, protocolId) || protocols.has(protocolId)) {
        throw new TypeError('tsjs');
      }
      const release = Reflect.apply(original, candidate, [protocol]);
      if (typeof release !== 'function') {
        throw new TypeError('tsjs');
      }
      protocols.set(protocolId, protocol);
      let live = true;
      return () => {
        if (!live) return;
        live = false;
        if (protocols.get(protocolId) === protocol) protocols.delete(protocolId);
        Reflect.apply(release, undefined, []);
      };
    };
    Object.defineProperties(captured, {
      ...descriptors,
      register: {
        ...register,
        value: wrappedRegister,
      },
    });
    return Object.freeze(captured);
  } catch {
    return candidate;
  }
}

class FirstDisplayAgentOwner implements FirstDisplayAgent {
  private readonly agentBatch: FirstDisplayBatchV1 | undefined;
  private readonly slotResults = new Map<
    string,
    FirstDisplayTerminalResult | 'no_bid' | 'failed' | 'cancelled'
  >();
  private readonly pending = new Set<string>();
  private readonly reasons = new Map<string, string | null>();
  private readonly handoffOwner: FirstDisplayHandoffOwner | undefined;
  private stateValue: FirstDisplayAgentState = 'ready';
  private mutationObserver: MutationObserver | undefined;
  private observedMutationRevision: number;
  private displayWasCommitted = false;
  private sealed = false;
  private disposedDriver = false;
  private failed = false;
  private actionStarted = false;
  private handoffFinalized = false;
  private committedArtifactsDetached = false;
  private lastTimingMs: number;
  private firstActionAtMs: number | null = null;
  private terminalAtMs: number | undefined;
  private paintAtMs: number | undefined;
  private nextTraceSequence = 1;
  private readonly acceptedTrace = new Map<
    string,
    Readonly<{ atMs: number; historySequence: number }>
  >();

  public constructor(private readonly options: FirstDisplayAgentOptions) {
    this.agentBatch = acceptServerFirstDisplayBatchV1(options.batch);
    this.observedMutationRevision = options.initialMutationRevision ?? 0;
    this.lastTimingMs = options.bootstrap.startedAtMs;
    this.handoffOwner = options.handoff
      ? createFirstDisplayHandoffOwner({
          releaseId: options.handoff.releaseId,
          generation: options.handoff.generation,
          initialMutationRevision: this.observedMutationRevision,
          isCurrentGeneration: () => !this.failed && this.stateValue !== 'disposed',
          isTerminal: () => this.stateValue === 'painted',
          isPainted: () => this.stateValue === 'painted',
          closeIngress: () => {
            if (!this.options.driver.closeIngress()) {
              throw new TypeError('tsjs');
            }
            this.closeNativeMutationIngress();
          },
          onFailure: (reason) => {
            this.fail(reason);
          },
        })
      : undefined;
    this.installNativeMutationIngress();
  }

  public get state(): FirstDisplayAgentState {
    return this.stateValue;
  }

  public coversProtocols(protocols: ReadonlyMap<FirstDisplayAuctionProtocolId, unknown>): boolean {
    return Boolean(
      this.agentBatch &&
      protocols.size === this.agentBatch.requiredProtocols.length &&
      this.agentBatch.requiredProtocols.every((id) => protocolIdentity(protocols.get(id), id))
    );
  }

  public start(): boolean {
    if (this.stateValue !== 'ready') return false;
    if (!this.agentBatch) return this.fail('abi_mismatch');
    if (!this.options.bootstrap.registerAgent()) {
      this.stateValue = 'failed';
      return false;
    }

    const actions: FirstDisplayBatchOutcomeV1[] = [];
    for (let index = 0; index < this.agentBatch.outcomes.length; index += 1) {
      const outcome = this.agentBatch.outcomes[index]!;
      if (ACTION_KINDS.has(outcome.kind)) {
        this.pending.add(outcome.slotId);
        actions.push(outcome);
      } else {
        this.slotResults.set(outcome.slotId, outcome.kind as 'no_bid' | 'failed' | 'cancelled');
        const decision = this.agentBatch.projection.auction.results[index];
        this.reasons.set(outcome.slotId, decision?.outcome === 'failed' ? decision.reason : null);
      }
    }
    this.stateValue = 'active';
    if (actions.length === 0) {
      this.recordTerminal();
      return true;
    }
    try {
      this.options.driver.start(
        Object.freeze(actions),
        () => this.recordFirstAction(),
        (slotId, result, reason) => {
          this.settle(slotId, result, reason);
        }
      );
      return true;
    } catch {
      return this.fail('bundle_partial');
    }
  }

  public admitTsBid(complete: () => void): boolean {
    if (!this.sealed || typeof complete !== 'function') return false;
    try {
      complete();
    } catch {
      // Bidder completion isolation cannot reopen sealed TS admission.
    }
    try {
      this.options.onPrebidAdmissionFailure?.('prebid_admission_failed');
    } catch {
      // Diagnostics cannot alter the terminal no-bid completion.
    }
    return false;
  }

  public observeNativeMutation(): boolean {
    if (this.stateValue !== 'painted' || this.failed) return false;
    if (this.handoffOwner) {
      const observed = this.handoffOwner.observeMutation();
      this.observedMutationRevision = this.handoffOwner.mutationRevision;
      return observed;
    }
    if (this.observedMutationRevision >= MAX_U32) return this.fail('bundle_partial');
    this.observedMutationRevision += 1;
    return true;
  }

  public snapshot(): FirstDisplayAgentSnapshotV1 {
    const outcomes = this.agentBatch?.outcomes.map(({ slotId }) =>
      Object.freeze({
        slotId,
        result: this.slotResults.get(slotId) ?? 'failed',
      })
    );
    return Object.freeze({
      state: this.stateValue,
      mutationRevision: this.observedMutationRevision,
      initialDisplayCommitted: this.displayWasCommitted,
      outcomes: Object.freeze(outcomes ?? []),
    });
  }

  public finalizeHandoff(): FinalizedFirstDisplayHandoffV1 | undefined {
    if (
      this.stateValue !== 'painted' ||
      this.failed ||
      !this.handoffOwner ||
      this.handoffFinalized
    ) {
      return undefined;
    }
    const finalized = this.handoffOwner.finalize(() => {
      const captured = this.options.driver.captureHandoff();
      if (!captured) return undefined;
      const candidate = this.captureHandoffData(captured);
      return candidate ? Object.freeze({ candidate, identities: captured.identities }) : undefined;
    });
    if (!finalized) return undefined;
    this.handoffFinalized = true;
    return finalized;
  }

  public detachCommittedArtifacts(): boolean {
    if (
      !this.handoffFinalized ||
      this.committedArtifactsDetached ||
      this.stateValue !== 'painted'
    ) {
      return false;
    }
    if (!this.options.driver.detachCommittedArtifacts()) return false;
    this.committedArtifactsDetached = true;
    return true;
  }

  public dispose(): void {
    if (this.stateValue === 'disposed') return;
    this.stateValue = 'disposed';
    this.handoffOwner?.dispose();
    this.disposeDriver();
    this.pending.clear();
  }

  private settle(slotId: string, result: FirstDisplayTerminalResult, reason: string | null): void {
    if (!this.actionStarted) {
      this.fail('bundle_partial');
      return;
    }
    if (this.stateValue !== 'active') return;
    if (
      typeof result !== 'string' ||
      !TERMINAL_RESULTS.has(result) ||
      (result === 'accepted'
        ? reason !== null
        : typeof reason !== 'string' || reason.length === 0 || reason.length > 256)
    ) {
      this.fail('bundle_partial');
      return;
    }
    if (!this.pending.has(slotId)) {
      if (!this.slotResults.has(slotId)) this.fail('bundle_partial');
      return;
    }
    if (result === 'accepted') {
      const atMs = this.readTiming();
      if (atMs === undefined || this.nextTraceSequence > 4_294_967_295) {
        this.fail('bundle_partial');
        return;
      }
      this.acceptedTrace.set(
        slotId,
        Object.freeze({ atMs, historySequence: this.nextTraceSequence })
      );
      this.nextTraceSequence += 1;
    }
    this.pending.delete(slotId);
    this.slotResults.set(slotId, result);
    this.reasons.set(slotId, reason);
    if (result === 'accepted') this.displayWasCommitted = true;
    if (this.pending.size === 0) this.recordTerminal();
  }

  private recordTerminal(): void {
    if (this.stateValue !== 'active' || this.pending.size !== 0) return;
    this.stateValue = 'terminal';
    this.terminalAtMs = this.readTiming();
    if (this.terminalAtMs === undefined) {
      this.fail('bundle_partial');
      return;
    }
    this.options.bootstrap.settle();
    this.mark('tsjs:first-display-terminal');
    this.scheduleProtectedPaint(2);
  }

  private recordFirstAction(): boolean {
    if (this.stateValue !== 'active' || this.actionStarted) return this.fail('bundle_partial');
    if (!this.options.bootstrap.startAction()) {
      if (this.options.bootstrap.state !== 'failed') return this.fail('bundle_partial');
      this.failed = true;
      this.stateValue = 'failed';
      this.disposeDriver();
      this.pending.clear();
      return false;
    }
    this.actionStarted = true;
    const firstDisplayMs = this.readTiming();
    if (firstDisplayMs === undefined) return this.fail('bundle_partial');
    this.firstActionAtMs = firstDisplayMs;
    this.mark('tsjs:first-display');
    try {
      this.options.performance.measure?.(
        'tsjs:boot-to-first-display',
        'tsjs:bids-script',
        'tsjs:first-display'
      );
    } catch {
      // Timing observability cannot alter display ownership.
    }
    return true;
  }

  private scheduleProtectedPaint(remaining: number): void {
    const next = (): void => {
      if (this.stateValue !== 'terminal') return;
      if (remaining > 1) {
        this.scheduleProtectedPaint(remaining - 1);
        return;
      }
      try {
        this.options.driver.sealTsAdmission();
      } catch {
        this.fail('bundle_partial');
        return;
      }
      this.sealed = true;
      this.stateValue = 'painted';
      this.paintAtMs = this.readTiming();
      if (this.paintAtMs === undefined) {
        this.fail('bundle_partial');
        return;
      }
      this.mark('tsjs:first-display-paint');
      try {
        this.options.onProtectedPaint();
      } catch {
        this.fail('bundle_partial');
      }
    };
    try {
      if (this.options.paint.hidden()) this.options.paint.scheduleHidden(next);
      else this.options.paint.requestFrame(next);
    } catch {
      this.fail('bundle_partial');
    }
  }

  private mark(name: string): void {
    try {
      this.options.performance.mark(name);
    } catch {
      // Timing observability cannot alter display ownership.
    }
  }

  private readTiming(): number | undefined {
    try {
      const value = this.options.now?.() ?? this.options.bootstrap.startedAtMs;
      if (!Number.isFinite(value) || value < 0 || value < this.lastTimingMs) return undefined;
      this.lastTimingMs = value;
      return value;
    } catch {
      return undefined;
    }
  }

  private captureHandoffData(captured: FirstDisplayDriverHandoffV1): unknown | undefined {
    const handoff = this.options.handoff;
    const batch = this.agentBatch;
    if (!handoff || !batch || this.terminalAtMs === undefined || this.paintAtMs === undefined) {
      return undefined;
    }
    const cycleBySlot = new Map(captured.cycles.map((cycle) => [cycle.slotId, cycle]));
    const diagnosticCycleBySlot = new Map(
      captured.diagnosticCycles.map((cycle) => [cycle.slotId, cycle])
    );
    const cycleTokenBySlot = new Map(
      captured.cycles.map((cycle) => [cycle.slotId, cycle.traceToken])
    );
    const artifactBySlot = new Map(
      captured.artifacts.map((artifact) => [artifact.slotId, artifact])
    );
    if (
      cycleBySlot.size !== captured.cycles.length ||
      diagnosticCycleBySlot.size !== captured.diagnosticCycles.length ||
      captured.diagnosticCycles.length !== captured.cycles.length ||
      artifactBySlot.size !== captured.artifacts.length
    ) {
      return undefined;
    }
    const bidBySlot = new Map(batch.projection.bids.map((bid) => [bid.slot, bid]));
    const slots = batch.projection.slots.map((placement, index) => {
      const outcome = this.slotResults.get(placement.slot);
      const projected = batch.outcomes[index];
      if (!outcome || !projected || projected.slotId !== placement.slot) {
        throw new TypeError('tsjs');
      }
      const cycle = cycleBySlot.get(placement.slot);
      const bid = bidBySlot.get(placement.slot);
      const targeting: Record<string, string> = { ...placement.targeting };
      if (bid) {
        Object.assign(targeting, bid.targeting);
        targeting.hb_adid = bid.rendererReservationId;
      }
      const committedArtifact =
        outcome === 'accepted' && (projected.kind === 'gpt_adm' || projected.kind === 'aps')
          ? projected.kind
          : 'none';
      return {
        id: placement.slot,
        aliases: [],
        domId: cycle?.element.id ?? placement.divId,
        gamPath: placement.gamUnitPath,
        formats: placement.formats.map((format) => [...format]),
        owner: cycle?.ownership ?? 'trusted_server',
        outcome,
        targeting: Object.keys(targeting)
          .sort()
          .map((key) => [key, targeting[key]]),
        committedArtifact,
        gptToken: cycleTokenBySlot.get(placement.slot) ?? null,
      };
    });
    const acceptedIds = new Set(
      slots.filter((slot) => slot.outcome === 'accepted').map((slot) => slot.id)
    );
    if (
      captured.cycles.some(({ slotId }) => !acceptedIds.has(slotId)) ||
      captured.artifacts.some((artifact) => {
        const projected = batch.outcomes.find(({ slotId }) => slotId === artifact.slotId);
        const bid = bidBySlot.get(artifact.slotId);
        return (
          !acceptedIds.has(artifact.slotId) ||
          !projected ||
          projected.kind !== artifact.kind ||
          bid?.rendererReservationId !== artifact.token
        );
      })
    ) {
      return undefined;
    }
    const issuer = this.options.identityIssuer;
    if (!issuer) return undefined;
    const attempts = [];
    for (let index = 0; index < slots.length; index += 1) {
      const slot = slots[index]!;
      const identity = issuer.mintAttemptId();
      if (!identity.ok) return undefined;
      attempts.push({
        id: identity.value,
        slotId: slot.id,
        ordinal: index + 1,
        state: slot.outcome,
        reason: this.reasons.get(slot.id) ?? null,
      });
    }
    const artifacts = slots
      .filter((slot) => slot.committedArtifact !== 'none')
      .map((slot) => {
        const bid = bidBySlot.get(slot.id);
        const capturedArtifact = artifactBySlot.get(slot.id);
        if (!bid || !capturedArtifact) {
          throw new TypeError('tsjs');
        }
        return {
          slotId: slot.id,
          kind: slot.committedArtifact,
          owner: capturedArtifact.owner,
          token: bid.rendererReservationId,
        };
      });
    const cycles = captured.cycles.map((cycle) => {
      const diagnostic = diagnosticCycleBySlot.get(cycle.slotId);
      if (!diagnostic || diagnostic.token !== cycle.traceToken) {
        throw new TypeError('tsjs');
      }
      return {
        slotId: diagnostic.slotId,
        token: diagnostic.token,
        nextCycleOrdinal: diagnostic.nextCycleOrdinal,
        unknownPriorCycle: diagnostic.unknownPriorCycle,
        records: diagnostic.records.map((record) => ({
          ordinal: record.ordinal,
          responseIdentifier: record.responseIdentifier,
          seen: [...record.seen],
          state: record.state,
        })),
        quarantines: [...diagnostic.quarantines],
      };
    });
    const traceSlots = slots.map((slot) => {
      const accepted = this.acceptedTrace.get(slot.id);
      return {
        slotId: slot.id,
        impressions: accepted ? 1 : 0,
        bindings:
          !accepted || slot.gptToken === null
            ? []
            : [
                {
                  atMs: accepted.atMs,
                  cycleOrdinal: 1,
                  historySequence: accepted.historySequence,
                  state: 'completed',
                  token: slot.gptToken,
                },
              ],
      };
    });
    return {
      version: 1,
      releaseId: handoff.releaseId,
      generation: handoff.generation,
      projectionDigest: batch.projectionDigest,
      slices: [...handoff.slices],
      slots,
      attempts,
      tombstones: captured.tombstones.map((entry) => ({ ...entry })),
      artifacts,
      parserState: this.options.parserState?.() ?? [],
      gptDiagnostics: Object.freeze({
        facts: Object.freeze([...captured.gptDiagnostics.facts]),
        overflowCount: captured.gptDiagnostics.overflowCount,
        dropCount: captured.gptDiagnostics.dropCount,
      }),
      timing: {
        bidsScriptMs: this.options.bootstrap.startedAtMs,
        firstDisplayMs: this.firstActionAtMs,
        terminalMs: this.terminalAtMs,
        paintMs: this.paintAtMs,
      },
      highWater: {
        navigationAttemptPrefix: issuer.snapshotPrefix(),
        nextNavigationAttemptOrdinal: attempts.length + 1,
        nextAttemptOrdinal: attempts.length + 1,
        nextSlotRegistrationOrdinal: slots.length + 1,
        reservationClockEpochMs: captured.clockEpochMs,
        nextReservationOrdinal: captured.nextReservationOrdinal,
        nextTicketOrdinal: captured.nextTicketOrdinal,
      },
      cycles,
      trace: {
        nextSequence: this.nextTraceSequence,
        nextGlobalSlotOrdinal: captured.nextTraceTokenOrdinal,
        slots: traceSlots,
      },
      mutationRevision: this.observedMutationRevision,
    };
  }

  private fail(reason: BootFailureReason): false {
    if (this.failed || this.stateValue === 'disposed') return false;
    this.failed = true;
    this.stateValue = 'failed';
    if (!this.options.bootstrap.fail(reason)) {
      try {
        this.options.onFailure(reason);
      } catch {
        // Post-paint failure publication cannot restore provisional authority.
      }
    }
    this.disposeDriver();
    this.pending.clear();
    return false;
  }

  private disposeDriver(): void {
    if (this.disposedDriver) return;
    this.disposedDriver = true;
    this.disposeNativeMutationIngress();
    try {
      this.options.driver.dispose();
    } catch {
      // Disposal is generation-latched; physical cleanup failure cannot restore authority.
    }
  }

  private installNativeMutationIngress(): void {
    if (!this.handoffOwner || !this.options.mutationDocument) return;
    try {
      const document = this.options.mutationDocument;
      const Observer = document.defaultView?.MutationObserver;
      if (!Observer || !document.documentElement) throw new TypeError('tsjs');
      const observer = new Observer((records) => this.observeDomMutations(records));
      observer.observe(document.documentElement, {
        attributes: true,
        childList: true,
        subtree: true,
      });
      this.mutationObserver = observer;
    } catch {
      this.fail('bundle_partial');
    }
  }

  private observeDomMutations(records: readonly MutationRecord[]): void {
    for (const record of records) {
      if (this.isOwnedRuntimeInsertion(record)) continue;
      if (!this.observeNativeMutation()) return;
    }
  }

  private isOwnedRuntimeInsertion(record: MutationRecord): boolean {
    if (record.type !== 'childList' || record.removedNodes.length !== 0) return false;
    const added = [...record.addedNodes];
    return (
      added.length === 1 &&
      added[0]?.nodeType === 1 &&
      (added[0] as Element).tagName === 'SCRIPT' &&
      (added[0] as Element).id === 'trustedserver-js-runtime'
    );
  }

  private closeNativeMutationIngress(): void {
    const observer = this.mutationObserver;
    this.mutationObserver = undefined;
    if (!observer) return;
    try {
      const records = observer.takeRecords();
      observer.disconnect();
      this.observeDomMutations(records);
    } catch {
      throw new TypeError('tsjs');
    }
  }

  private disposeNativeMutationIngress(): void {
    const observer = this.mutationObserver;
    this.mutationObserver = undefined;
    if (!observer) return;
    try {
      observer.disconnect();
    } catch {
      // Generation latching makes a failed physical observer removal inert.
    }
  }
}

/** Create the bounded provisional lifecycle owner; it publishes no runtime API. */
export function createFirstDisplayAgent(options: FirstDisplayAgentOptions): FirstDisplayAgent {
  const owner = new FirstDisplayAgentOwner(options);
  return Object.freeze({
    get state() {
      return owner.state;
    },
    coversProtocols: (protocols: ReadonlyMap<FirstDisplayAuctionProtocolId, unknown>) =>
      owner.coversProtocols(protocols),
    start: () => owner.start(),
    admitTsBid: (complete: () => void) => owner.admitTsBid(complete),
    observeNativeMutation: () => owner.observeNativeMutation(),
    snapshot: () => owner.snapshot(),
    finalizeHandoff: () => owner.finalizeHandoff(),
    detachCommittedArtifacts: () => owner.detachCommittedArtifacts(),
    dispose: () => owner.dispose(),
  });
}

function prepareRegisteredAgent(host: unknown): PreparedFirstDisplayBaseV1 {
  try {
    if (
      typeof host !== 'object' ||
      host === null ||
      Array.isArray(host) ||
      Object.getPrototypeOf(host) !== Object.prototype ||
      !Object.isFrozen(host) ||
      Reflect.ownKeys(host).length !== 2
    ) {
      throw new TypeError('tsjs');
    }
    const descriptor = Object.getOwnPropertyDescriptor(host, 'options');
    const bindingsDescriptor = Object.getOwnPropertyDescriptor(host, 'sliceBindings');
    if (
      !descriptor?.enumerable ||
      !('value' in descriptor) ||
      typeof descriptor.value !== 'object' ||
      descriptor.value === null ||
      !Object.isFrozen(descriptor.value) ||
      !bindingsDescriptor?.enumerable ||
      !('value' in bindingsDescriptor) ||
      typeof bindingsDescriptor.value !== 'function'
    ) {
      throw new TypeError('tsjs');
    }
    const options = descriptor.value as FirstDisplayAgentRegistrationHostV1['options'];
    const sliceBindings = bindingsDescriptor.value as (id: string) => unknown;
    const auctionProtocols = new Map<FirstDisplayAuctionProtocolId, unknown>();
    const fullProtocols = new Map<FirstDisplayAuctionProtocolId, unknown>();
    const parserState = createFirstDisplayParserStateCollector();
    let agent: FirstDisplayAgent | undefined;
    const sliceHost: FirstDisplaySliceHost = Object.freeze({
      activate: (
        id: OptionalFirstDisplaySliceId,
        own: FirstDisplaySliceActivationContext['own'],
        install?: InitialSliceInstaller
      ): void => {
        if (typeof install !== 'function') {
          throw new TypeError('tsjs');
        }
        if (!parserState.register(id)) {
          throw new TypeError('tsjs');
        }
        const protocolId = id.endsWith('_initial')
          ? (id.slice(0, -'_initial'.length) as FirstDisplayAuctionProtocolId)
          : undefined;
        const candidate = captureMutationObservedBindings(
          sliceBindings(id),
          () => agent?.observeNativeMutation() === true,
          (key, value) => {
            if (!parserState.observe(id, key, value)) {
              throw new TypeError('tsjs');
            }
          }
        );
        const installed = install(
          protocolId && AUCTION_PROTOCOLS.includes(protocolId)
            ? captureProtocolRegistration(candidate, protocolId, fullProtocols)
            : candidate,
          own
        );
        if (protocolId && AUCTION_PROTOCOLS.includes(protocolId)) {
          if (auctionProtocols.has(protocolId) || !protocolIdentity(installed, protocolId)) {
            throw new TypeError('tsjs');
          }
          auctionProtocols.set(protocolId, installed);
        }
      },
    });
    return Object.freeze({
      activate: (context: FirstDisplaySliceActivationContext): void => {
        const rendererOwner: { value: FirstDisplayRenderBridgeV1 | undefined } = {
          value: undefined,
        };
        context.own(() => {
          if (agent) agent.dispose();
          else rendererOwner.value?.dispose();
        });
        context.afterActivate(() => {
          const batch = acceptServerFirstDisplayBatchV1(options.batch);
          if (!batch) throw new TypeError('tsjs');
          const gpt = fullProtocols.get('gpt');
          const aps = fullProtocols.get('aps');
          if (batch.requiredProtocols.includes('gpt') && !fullProtocolIdentity(gpt, 'gpt')) {
            throw new TypeError('tsjs');
          }
          if (batch.requiredProtocols.includes('aps') && !fullProtocolIdentity(aps, 'aps')) {
            throw new TypeError('tsjs');
          }
          const renderOptions: Omit<FirstDisplayRenderBridgeOptionsV1, 'getAps'> = {
            browser: options.gptInput.browser,
            clearTimer: options.gptInput.clearTimer,
            createChannel: () => createBrowserMessageChannel(options.gptInput.browser),
            document: options.gptInput.document,
            fillRandom: (bytes) => fillBrowserRandom(options.gptInput.browser, bytes),
            now: () => readBrowserNow(options.gptInput.browser),
            onNativeMutation: () => agent?.observeNativeMutation() === true,
            setTimer: options.gptInput.setTimer,
          };
          const apsProtocol = fullProtocolIdentity(aps, 'aps')
            ? (aps as FirstDisplayApsProtocolV1)
            : undefined;
          const renderer = apsProtocol
            ? apsProtocol.createRenderBridge(renderOptions)
            : createFirstDisplayAdmRenderBridge(renderOptions);
          rendererOwner.value = renderer;
          const driver = createFirstDisplayProjectedDriver({
            batch,
            ...(gpt ? { gpt: gpt as FirstDisplayGptProtocolV1 } : {}),
            gptInput: {
              ...options.gptInput,
              onNativeMutation: () => agent?.observeNativeMutation() === true,
            },
            renderer,
          });
          const identityIssuer = createNavigationIdentityIssuerFromSource((target) => {
            fillBrowserRandom(options.gptInput.browser, target);
            return target;
          });
          if (!identityIssuer.ok) {
            throw new TypeError('tsjs');
          }
          agent = createFirstDisplayAgent({
            ...options,
            driver,
            identityIssuer: identityIssuer.value,
            mutationDocument: options.gptInput.document,
            parserState: parserState.snapshot,
          });
          if (!agent.coversProtocols(auctionProtocols)) {
            throw new TypeError('tsjs');
          }
          if (!agent.start()) throw new TypeError('tsjs');
          options.onAgentReady?.(agent);
        });
      },
      sliceHost,
    });
  } catch {
    throw new TypeError('tsjs');
  }
}

registerCurrentFirstDisplayComponent('first_display', 1, prepareRegisteredAgent);
