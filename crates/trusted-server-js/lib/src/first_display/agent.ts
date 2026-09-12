import type { BootFailureReason } from '../kernel/fallback';
import type { FirstDisplayNavigationIdentityIssuer } from '../kernel/identity';
import type { FirstDisplaySliceId } from '../kernel/release_catalog';
import type {
  FirstDisplayGptDiagnosticEventV1,
  FirstDisplayGptDiagnosticsV1,
} from '../shared/takeover';
import type { FirstDisplaySliceActivationContext } from '../shared/first_display_transaction';
import type {
  FinalizedFirstDisplayHandoffV1,
  FirstDisplayAgentCaptureFinalizerV1,
} from '../shared/first_display_handoff';

import type {
  FirstDisplayGoogletagBatchInput,
  FirstDisplayGptBoundCycleV1,
  FirstDisplayGptHandoffCycleV1,
} from './adapters/googletag';
import type { FirstDisplayRenderBridgeCapabilityV1 } from './driver';
import type { FirstDisplayApsProtocolV1 } from './leaf/aps_protocol';
import type { FirstDisplayGptCapabilityV1, FirstDisplayGptProtocolV1 } from './leaf/gpt_protocol';
import type {
  FirstDisplayRenderOwnerOptionsV1,
  FirstDisplayRenderOwnerProtocolV1,
} from './render_journal';
import type {
  FirstDisplaySliceHost,
  InitialSliceInstaller,
  OptionalFirstDisplaySliceId,
} from './slices/definition';
import {
  acceptServerFirstDisplayBatchV1,
  type FirstDisplayAgentBatchV1,
  type FirstDisplayAuctionProtocolId,
  type FirstDisplayBatchOutcomeV1,
} from './leaf/projection';

const MAX_U32 = 4_294_967_295;
const BUNDLE_PARTIAL: BootFailureReason = 'bundle_partial';
const AUCTION_PROTOCOLS = ['aps', 'gpt', 'prebid'] as const;
const SLICE_PROTOCOLS = ['render_owner', ...AUCTION_PROTOCOLS] as const;
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
  readonly sweepCommittedArtifacts: () => number;
  readonly dispose: () => void;
}

export interface FirstDisplayDriverHandoffV1 {
  readonly artifacts: readonly Readonly<{
    hostPosition: string | null;
    hostPositionPriority: string | null;
    identity: object;
    kind: 'gpt_adm' | 'aps';
    owner: 'trusted_server' | 'publisher';
    slotId: string;
    token: string;
  }>[];
  readonly cycles: readonly FirstDisplayGptHandoffCycleV1[];
  readonly diagnosticCycles: readonly Readonly<{
    nextCycleOrdinal: number;
    quarantines: readonly string[];
    records: readonly Readonly<{
      ordinal: number;
      responseIdentifier: string | null;
      seen: readonly FirstDisplayGptDiagnosticEventV1[];
      state: 'open' | 'completed' | 'retired';
    }>[];
    slotId: string;
    token: string;
    unknownPriorCycle: boolean;
  }>[];
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
  readonly production?: Readonly<{
    gpt?: FirstDisplayGptProtocolV1;
    gptInput: readonly [
      browser: FirstDisplayGoogletagBatchInput[0],
      clearTimer: FirstDisplayGoogletagBatchInput[1],
      document: FirstDisplayGoogletagBatchInput[2],
      setTimer: FirstDisplayGoogletagBatchInput[3],
      diagnosticsActive?: FirstDisplayGoogletagBatchInput[5],
      onNativeMutation?: FirstDisplayGoogletagBatchInput[6],
    ];
    renderer?: FirstDisplayRenderBridgeCapabilityV1;
  }>;
  readonly startedAtMs: number;
  readonly performance: Readonly<{
    mark: (name: string) => void;
    measure?: (name: string, startMark: string, endMark: string) => void;
  }>;
  readonly paint: FirstDisplayPaintScheduler;
  readonly onProtectedPaint: () => void;
  readonly onSettled: () => void;
  readonly onFailure: (reason: BootFailureReason) => void;
  readonly mutationDocument?: Document;
  readonly initialMutationRevision?: number;
  readonly now?: () => number;
  readonly identityIssuer?: FirstDisplayNavigationIdentityIssuer;
  readonly parserState?: () => readonly (readonly [
    string,
    readonly (readonly [string, string | number | boolean | null])[],
  ])[];
  readonly handoff?: Readonly<{
    releaseId: string;
    generation: number;
    integrationConfigDigest: string;
    slices: readonly FirstDisplaySliceId[];
  }>;
}

/** Bootstrap-owned dependencies supplied only to the release-bound base component. */
export interface FirstDisplayAgentRegistrationHostV1 {
  readonly options: FirstDisplayAgentOptions &
    Readonly<{
      gptInput: readonly [
        browser: FirstDisplayGoogletagBatchInput[0],
        clearTimer: FirstDisplayGoogletagBatchInput[1],
        document: FirstDisplayGoogletagBatchInput[2],
        setTimer: FirstDisplayGoogletagBatchInput[3],
        diagnosticsActive?: FirstDisplayGoogletagBatchInput[5],
      ];
      onAgentReady?: (agent: FirstDisplayAgent) => void;
    }>;
  readonly sliceBindings: (
    id: string,
    observe: (key: unknown, value: unknown) => void,
    register: ((protocol: unknown) => () => void) | undefined
  ) => readonly [bindings: unknown, config: unknown];
}

function createBrowserMessageChannel(
  browser: Window
): ReturnType<FirstDisplayRenderOwnerOptionsV1[2]> {
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
  return { port1, port2 } as ReturnType<FirstDisplayRenderOwnerOptionsV1[2]>;
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

export interface PreparedFirstDisplayBaseV1 {
  readonly activate: (context: FirstDisplaySliceActivationContext) => void;
  readonly sliceHost: FirstDisplaySliceHost;
}

export interface FirstDisplayAgent {
  readonly state: FirstDisplayAgentState;
  readonly mutationRevision: number;
  readonly initialDisplayCommitted: boolean;
  readonly start: () => boolean;
  readonly observeNativeMutation: () => boolean;
  readonly finalizeHandoff: (
    finalize: FirstDisplayAgentCaptureFinalizerV1
  ) => FinalizedFirstDisplayHandoffV1 | undefined;
  readonly detachCommittedArtifacts: () => boolean;
  readonly dispose: () => void;
}

type FirstDisplayRegisteredProtocolId = FirstDisplayAuctionProtocolId | 'render_owner';

function protocolIdentity(
  candidate: unknown,
  expected: FirstDisplayRegisteredProtocolId,
  exact = true
): boolean {
  try {
    const length = exact ? 2 : 3;
    if (
      !Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Array.prototype ||
      !Object.isFrozen(candidate) ||
      candidate.length !== length ||
      Reflect.ownKeys(candidate).length !== length + 1
    ) {
      return false;
    }
    for (let index = 0; index < length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(candidate, String(index));
      if (!descriptor?.enumerable || !('value' in descriptor)) return false;
    }
    return candidate[0] === 1 && candidate[1] === expected && (exact || candidate[2] !== undefined);
  } catch {
    return false;
  }
}

function fullProtocolIdentity(
  candidate: unknown,
  expected: FirstDisplayRegisteredProtocolId
): boolean {
  return protocolIdentity(candidate, expected, false);
}

function sameCycle(
  expected: FirstDisplayGptBoundCycleV1,
  candidate: FirstDisplayGptBoundCycleV1
): boolean {
  return candidate === expected;
}

class FirstDisplayAgentOwner implements FirstDisplayAgent {
  private readonly agentBatch: FirstDisplayAgentBatchV1 | undefined;
  private readonly slotResults = new Map<
    string,
    FirstDisplayTerminalResult | 'no_bid' | 'failed' | 'cancelled'
  >();
  private readonly pending = new Set<string>();
  private readonly reasons = new Map<string, string | null>();
  private handoffCapsule: FinalizedFirstDisplayHandoffV1['capsule'] | undefined;
  private stateValue: FirstDisplayAgentState = 'ready';
  private mutationObserver: MutationObserver | undefined;
  private observedMutationRevision: number;
  private displayWasCommitted = false;
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
  private readonly bound = new Map<string, FirstDisplayGptBoundCycleV1>();
  private productionBatch: FirstDisplayGptCapabilityV1 | undefined;

  public constructor(private readonly options: FirstDisplayAgentOptions) {
    this.agentBatch = acceptServerFirstDisplayBatchV1(options.batch);
    this.observedMutationRevision = options.initialMutationRevision ?? 0;
    this.lastTimingMs = options.startedAtMs;
    this.installNativeMutationIngress();
  }

  public get state(): FirstDisplayAgentState {
    return this.stateValue;
  }

  public get mutationRevision(): number {
    return this.observedMutationRevision;
  }

  public get initialDisplayCommitted(): boolean {
    return this.displayWasCommitted;
  }

  public start(): boolean {
    if (this.stateValue !== 'ready') return false;
    if (!this.agentBatch) return this.fail('abi_mismatch');
    const started = this.readTiming();
    if (started === undefined || started - this.options.startedAtMs >= 10_000) {
      return this.fail(BUNDLE_PARTIAL);
    }

    let actionCount = 0;
    const outcomes = this.agentBatch[2];
    const projection = this.agentBatch[3];
    for (let index = 0; index < outcomes.length; index += 1) {
      const outcome = outcomes[index]!;
      if (ACTION_KINDS.has(outcome[1])) {
        this.pending.add(outcome[0]);
        actionCount += 1;
      } else {
        this.slotResults.set(outcome[0], outcome[1] as 'no_bid' | 'failed' | 'cancelled');
        const decision = projection.auction.results[index];
        this.reasons.set(outcome[0], decision?.outcome === 'failed' ? decision.reason : null);
      }
    }
    this.stateValue = 'active';
    if (actionCount === 0) {
      this.recordTerminal();
      return true;
    }
    try {
      if (!this.startProduction()) {
        throw new TypeError('tsjs');
      }
      return true;
    } catch {
      return this.fail(BUNDLE_PARTIAL);
    }
  }

  public observeNativeMutation(): boolean {
    if (this.stateValue !== 'painted' || this.failed) return false;
    if (this.observedMutationRevision >= MAX_U32) return this.fail(BUNDLE_PARTIAL);
    this.observedMutationRevision += 1;
    return true;
  }

  public finalizeHandoff(
    finalize: FirstDisplayAgentCaptureFinalizerV1
  ): FinalizedFirstDisplayHandoffV1 | undefined {
    if (
      this.stateValue !== 'painted' ||
      this.failed ||
      !this.options.handoff ||
      this.handoffFinalized ||
      typeof finalize !== 'function'
    ) {
      return undefined;
    }
    try {
      if (!this.closeDriverIngress()) throw new TypeError('tsjs');
      this.closeNativeMutationIngress();
      const currentTimeMs = this.readTiming();
      if (
        currentTimeMs === undefined ||
        !this.agentBatch ||
        this.terminalAtMs === undefined ||
        this.paintAtMs === undefined
      ) {
        throw new TypeError('tsjs');
      }
      const finalized = finalize([
        this.options.handoff,
        Object.freeze([this.agentBatch[0], this.agentBatch[2]]),
        this.slotResults,
        this.reasons,
        this.acceptedTrace,
        this.options.parserState?.() ?? [],
        this.productionBatch?.[2](),
        this.productionBatch?.[3](),
        this.options.production?.renderer?.[7](),
        [
          this.options.startedAtMs,
          this.firstActionAtMs,
          this.terminalAtMs,
          this.paintAtMs,
          currentTimeMs,
        ],
        this.nextTraceSequence,
        this.observedMutationRevision,
      ]);
      if (!finalized) throw new TypeError('tsjs');
      this.handoffCapsule = finalized.capsule;
      this.handoffFinalized = true;
      return finalized;
    } catch {
      this.fail(BUNDLE_PARTIAL);
      return undefined;
    }
  }

  public detachCommittedArtifacts(): boolean {
    if (
      !this.handoffFinalized ||
      this.committedArtifactsDetached ||
      this.stateValue !== 'painted'
    ) {
      return false;
    }
    if (!this.detachDriverArtifacts()) return false;
    this.committedArtifactsDetached = true;
    return true;
  }

  public dispose(): void {
    if (this.stateValue === 'disposed') return;
    this.stateValue = 'disposed';
    this.handoffCapsule?.clear();
    this.handoffCapsule = undefined;
    this.disposeDriver();
    this.pending.clear();
  }

  private settle(slotId: string, result: FirstDisplayTerminalResult, reason: string | null): void {
    if (!this.actionStarted) {
      this.fail(BUNDLE_PARTIAL);
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
      this.fail(BUNDLE_PARTIAL);
      return;
    }
    if (!this.pending.has(slotId)) {
      if (!this.slotResults.has(slotId)) this.fail(BUNDLE_PARTIAL);
      return;
    }
    if (result === 'accepted') {
      const atMs = this.readTiming();
      if (atMs === undefined || this.nextTraceSequence > 4_294_967_295) {
        this.fail(BUNDLE_PARTIAL);
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
      this.fail(BUNDLE_PARTIAL);
      return;
    }
    try {
      this.options.onSettled();
    } catch {
      this.fail(BUNDLE_PARTIAL);
      return;
    }
    this.mark('tsjs:first-display-terminal');
    this.scheduleProtectedPaint(2);
  }

  private recordFirstAction(): boolean {
    if (this.stateValue !== 'active' || this.actionStarted) return this.fail(BUNDLE_PARTIAL);
    const firstDisplayMs = this.readTiming();
    if (firstDisplayMs === undefined || firstDisplayMs - this.options.startedAtMs >= 10_000) {
      return this.fail(BUNDLE_PARTIAL);
    }
    this.actionStarted = true;
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
        this.options.production?.renderer?.[5]();
      } catch {
        this.fail(BUNDLE_PARTIAL);
        return;
      }
      this.stateValue = 'painted';
      this.paintAtMs = this.readTiming();
      if (this.paintAtMs === undefined) {
        this.fail(BUNDLE_PARTIAL);
        return;
      }
      this.mark('tsjs:first-display-paint');
      try {
        this.options.onProtectedPaint();
      } catch {
        this.fail(BUNDLE_PARTIAL);
      }
    };
    try {
      if (this.options.paint.hidden()) this.options.paint.scheduleHidden(next);
      else this.options.paint.requestFrame(next);
    } catch {
      this.fail(BUNDLE_PARTIAL);
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
      const value = this.options.now?.() ?? this.options.startedAtMs;
      if (!Number.isFinite(value) || value < 0 || value < this.lastTimingMs) return undefined;
      this.lastTimingMs = value;
      return value;
    } catch {
      return undefined;
    }
  }

  private startProduction(): boolean {
    const production = this.options.production;
    const batch = this.agentBatch;
    const renderer = production?.renderer;
    if (!production || !batch || !renderer) return false;
    const gptBatch = production.gpt?.[2](
      Object.freeze([
        production.gptInput[0],
        production.gptInput[1],
        production.gptInput[2],
        production.gptInput[3],
        batch[3],
        production.gptInput[4],
        production.gptInput[5],
      ])
    );
    if (!gptBatch) return false;
    this.productionBatch = gptBatch;
    return gptBatch[0]([
      (cycle): void => {
        const action = batch[2].find(([slotId]) => slotId === cycle[6]);
        const bidIndex = batch[3].bids.indexOf(cycle[0]);
        const placement = batch[3].slots.find(({ slot }) => slot === cycle[6]);
        if (
          !action ||
          !ACTION_KINDS.has(action[1]) ||
          this.bound.has(cycle[6]) ||
          bidIndex < 0 ||
          batch[3].bids[bidIndex]?.slot !== cycle[6] ||
          cycle[5] !== placement
        ) {
          this.settle(cycle[6], 'failed', 'gpt_request_failed');
          return;
        }
        this.bound.set(cycle[6], cycle);
        if (!renderer[0](cycle, (result, reason) => this.settle(cycle[6], result, reason))) {
          this.settle(cycle[6], 'failed', 'internal_error');
        }
      },
      (slotId, reason): void => {
        const cycle = this.bound.get(slotId);
        if (cycle) renderer[2](cycle);
        this.settle(slotId, 'failed', reason);
      },
      () => this.recordFirstAction(),
      (cycle, result): void => {
        const exact = this.bound.get(cycle[6]);
        if (!exact || !sameCycle(exact, cycle) || !renderer[1](exact, result)) {
          this.settle(cycle[6], 'failed', 'gpt_request_failed');
        }
      },
      (cycle): void => {
        const exact = this.bound.get(cycle[6]);
        if (exact && sameCycle(exact, cycle)) renderer[3](exact);
      },
    ]);
  }

  private acceptedSlotIds(): string[] {
    return [...this.slotResults.entries()]
      .filter(([, result]) => result === 'accepted')
      .map(([slotId]) => slotId);
  }

  private closeDriverIngress(): boolean {
    const production = this.options.production;
    if (!production) return false;
    const accepted = this.acceptedSlotIds();
    return (
      (!this.productionBatch || this.productionBatch[1](accepted)) &&
      (!production.renderer || production.renderer[6]())
    );
  }

  private detachDriverArtifacts(): boolean {
    const production = this.options.production;
    if (!production) return false;
    const accepted = this.acceptedSlotIds();
    return (
      (!this.productionBatch || this.productionBatch[4](accepted)) &&
      (!production.renderer || production.renderer[8]())
    );
  }

  private fail(reason: BootFailureReason): false {
    if (this.failed || this.stateValue === 'disposed') return false;
    this.failed = true;
    this.stateValue = 'failed';
    try {
      this.options.onFailure(reason);
    } catch {
      // Failure publication cannot restore provisional authority.
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
      this.productionBatch?.[5]();
    } catch {
      // Every owned subsystem must still receive its independent disposal attempt.
    }
    try {
      this.options.production?.renderer?.[9]();
    } catch {
      // Disposal is generation-latched; physical cleanup failure cannot restore authority.
    }
    this.bound.clear();
  }

  private installNativeMutationIngress(): void {
    if (!this.options.handoff || !this.options.mutationDocument) return;
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
      this.fail(BUNDLE_PARTIAL);
    }
  }

  private observeDomMutations(records: readonly MutationRecord[]): void {
    if (records.length > 0) {
      try {
        this.options.production?.renderer?.[4]();
      } catch {
        this.fail(BUNDLE_PARTIAL);
        return;
      }
    }
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
  return new FirstDisplayAgentOwner(options);
}

/** Prepare the bootstrap-owned first-display coordinator for its optional slices. */
export function prepareFirstDisplayBase(
  host: FirstDisplayAgentRegistrationHostV1
): PreparedFirstDisplayBaseV1 {
  const { options, sliceBindings } = host;
  const fullProtocols = new Map<FirstDisplayRegisteredProtocolId, unknown>();
  const parserState = new Map<
    string,
    { observations: string[]; values: Map<string, string | number | boolean | null> }
  >();
  const registerParserSlice = (id: string): boolean => {
    if (parserState.has(id) || !options.handoff?.slices.includes(id as FirstDisplaySliceId)) {
      return false;
    }
    parserState.set(id, { observations: [], values: new Map() });
    return true;
  };
  const observeParserValue = (id: string, key: unknown, value: unknown): boolean => {
    const state = parserState.get(id);
    if (
      !state ||
      typeof key !== 'string' ||
      key.length === 0 ||
      key.length > 128 ||
      (value !== null &&
        typeof value !== 'string' &&
        typeof value !== 'boolean' &&
        !(typeof value === 'number' && Number.isFinite(value))) ||
      (typeof value === 'string' && value.length > 4_096)
    ) {
      return false;
    }
    if (!state.values.has(key)) {
      if (state.values.size >= 256) return false;
      state.observations.push(key);
    }
    state.values.set(key, value as string | number | boolean | null);
    return true;
  };
  const snapshotParserState = () =>
    Object.freeze(
      [...parserState].map(([sliceId, state]) =>
        Object.freeze([
          sliceId,
          Object.freeze(
            state.observations.map((key) => Object.freeze([key, state.values.get(key)!] as const))
          ),
        ] as const)
      )
    );
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
      if (!registerParserSlice(id)) {
        throw new TypeError('tsjs');
      }
      const protocolId = id.endsWith('_initial')
        ? (id.slice(0, -'_initial'.length) as FirstDisplayRegisteredProtocolId)
        : undefined;
      const observe = (key: unknown, value: unknown): void => {
        if (!observeParserValue(id, key, value)) throw new TypeError('tsjs');
        agent?.observeNativeMutation();
      };
      const register =
        protocolId && SLICE_PROTOCOLS.includes(protocolId)
          ? (protocol: unknown): (() => void) => {
              if (!fullProtocolIdentity(protocol, protocolId) || fullProtocols.has(protocolId)) {
                throw new TypeError('tsjs');
              }
              fullProtocols.set(protocolId, protocol);
              return () => {
                if (fullProtocols.get(protocolId) === protocol) {
                  fullProtocols.delete(protocolId);
                }
              };
            }
          : undefined;
      // The authenticated bootstrap builds the exact carrier around these
      // base-owned observation and protocol capabilities.
      const transported = sliceBindings(id, observe, register);
      if (!Array.isArray(transported) || transported.length !== 2) {
        throw new TypeError('tsjs');
      }
      const installed = install(transported[0], own, transported[1]);
      if (
        protocolId &&
        SLICE_PROTOCOLS.includes(protocolId) &&
        !protocolIdentity(installed, protocolId)
      ) {
        throw new TypeError('tsjs');
      }
    },
  });
  return Object.freeze({
    activate: (context: FirstDisplaySliceActivationContext): void => {
      const rendererOwner: { value: FirstDisplayRenderBridgeCapabilityV1 | undefined } = {
        value: undefined,
      };
      context.own(() => {
        if (agent) agent.dispose();
        else rendererOwner.value?.[9]();
      });
      context.afterActivate(() => {
        const batch = acceptServerFirstDisplayBatchV1(options.batch);
        if (!batch) throw new TypeError('tsjs');
        const gpt = fullProtocols.get('gpt');
        const aps = fullProtocols.get('aps');
        const renderOwner = fullProtocols.get('render_owner');
        const requiresRenderOwner = batch[2].some(([, kind]) => ACTION_KINDS.has(kind));
        const activatedAuctionProtocols = AUCTION_PROTOCOLS.filter((id) => fullProtocols.has(id));
        if (
          activatedAuctionProtocols.length !== batch[1].length ||
          !batch[1].every((id) => fullProtocolIdentity(fullProtocols.get(id), id))
        ) {
          throw new TypeError('tsjs');
        }
        if (
          requiresRenderOwner !== fullProtocolIdentity(renderOwner, 'render_owner') ||
          (aps !== undefined && renderOwner === undefined)
        ) {
          throw new TypeError('tsjs');
        }
        const renderOptions: FirstDisplayRenderOwnerOptionsV1 = Object.freeze([
          options.gptInput[0],
          options.gptInput[1],
          () => createBrowserMessageChannel(options.gptInput[0]),
          options.gptInput[2],
          (bytes) => fillBrowserRandom(options.gptInput[0], bytes),
          () => readBrowserNow(options.gptInput[0]),
          () => agent?.observeNativeMutation() === true,
          options.gptInput[3],
        ]);
        const apsProtocol = fullProtocolIdentity(aps, 'aps')
          ? (aps as FirstDisplayApsProtocolV1)
          : undefined;
        const renderStrategy = apsProtocol?.[2].createRenderStrategy(renderOptions);
        const renderer = renderOwner
          ? (renderOwner as FirstDisplayRenderOwnerProtocolV1)[2](renderOptions, renderStrategy)
          : undefined;
        rendererOwner.value = renderer;
        agent = createFirstDisplayAgent({
          ...options,
          mutationDocument: options.gptInput[2],
          parserState: snapshotParserState,
          production: {
            ...(gpt ? { gpt: gpt as FirstDisplayGptProtocolV1 } : {}),
            gptInput: Object.freeze([
              options.gptInput[0],
              options.gptInput[1],
              options.gptInput[2],
              options.gptInput[3],
              options.gptInput[4],
              () => agent?.observeNativeMutation() === true,
            ]),
            ...(renderer ? { renderer } : {}),
          },
        });
        if (!agent.start()) throw new TypeError('tsjs');
        options.onAgentReady?.(agent);
      });
    },
    sliceHost,
  });
}
