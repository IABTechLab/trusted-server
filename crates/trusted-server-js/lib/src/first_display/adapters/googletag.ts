import type {
  FirstDisplayProjectionBidV1,
  FirstDisplayProjectionSlotV1,
  FirstDisplayProjectionV1,
} from '../leaf/projection';
import type { FirstDisplayGptProtocolV1 } from '../leaf/gpt_protocol';
import type {
  FirstDisplayGptDiagnosticEventV1,
  FirstDisplayGptDiagnosticsV1,
  FirstDisplayGptFactV1,
} from '../../shared/takeover';

const MAX_DIAGNOSTIC_FACTS = 512;
const MAX_DIAGNOSTIC_FACT_BYTES = 1_000;
const MAX_DIAGNOSTIC_SECTION_BYTES = 512 * 1024;
const MAX_U32 = 4_294_967_295;
const SLOT_REQUESTED = 'slotRequested';
const SLOT_RESPONSE_RECEIVED = 'slotResponseReceived';
const SLOT_RENDER_ENDED = 'slotRenderEnded';
const SLOT_ONLOAD = 'slotOnload';
const IMPRESSION_VIEWABLE = 'impressionViewable';
const SLOT_VISIBILITY_CHANGED = 'slotVisibilityChanged';
const OVERLAPPING_REQUEST_CYCLES = 'overlapping_request_cycles';
const INVALID_EVENT_ORDER = 'invalid_event_order';
const GPT_REQUEST_FAILED = 'gpt_request_failed';
const SLOT_UNRESOLVED = 'slot_unresolved';
const DIAGNOSTIC_ONLY_EVENTS = Object.freeze([
  SLOT_RESPONSE_RECEIVED,
  SLOT_ONLOAD,
  IMPRESSION_VIEWABLE,
  SLOT_VISIBILITY_CHANGED,
] as const);
const DIAGNOSTIC_EVENT_ORDER: readonly FirstDisplayGptDiagnosticEventV1[] = Object.freeze([
  SLOT_REQUESTED,
  SLOT_RESPONSE_RECEIVED,
  SLOT_RENDER_ENDED,
  SLOT_ONLOAD,
  IMPRESSION_VIEWABLE,
  SLOT_VISIBILITY_CHANGED,
]);

export type FirstDisplayGptRenderResult = 'gam_empty' | 'nonempty_gam';
export type FirstDisplayGptFailureReason =
  | 'cycle_unattributable'
  | 'external_artifact_incompatible'
  | 'external_ready_timeout'
  | 'gpt_completion_timeout'
  | typeof GPT_REQUEST_FAILED
  | 'gpt_request_timeout'
  | typeof SLOT_UNRESOLVED;

export interface FirstDisplayGptBoundCycleV1 {
  readonly bid: FirstDisplayProjectionBidV1;
  readonly element: HTMLElement;
  /** Closure-private physical-cycle validity; false after a later GPT request takes the slot. */
  readonly isCurrent: () => boolean;
  readonly ownership: 'publisher' | 'trusted_server';
  readonly physicalSlot: object;
  readonly placement: FirstDisplayProjectionSlotV1;
  readonly slotId: string;
  readonly traceToken: string;
}

export interface FirstDisplayTargetingOwnershipV1 {
  readonly installed: string;
  readonly key: string;
  readonly prior: readonly string[];
}

export interface FirstDisplayGptHandoffCycleV1 extends FirstDisplayGptBoundCycleV1 {
  readonly targetingOwnership: readonly FirstDisplayTargetingOwnershipV1[];
}

export interface FirstDisplayGptDiagnosticCycleV1 {
  readonly nextCycleOrdinal: number;
  readonly quarantines: readonly string[];
  readonly records: readonly Readonly<{
    readonly ordinal: number;
    readonly responseIdentifier: string | null;
    readonly seen: readonly FirstDisplayGptDiagnosticEventV1[];
    readonly state: 'open' | 'completed' | 'retired';
  }>[];
  readonly slotId: string;
  readonly token: string;
  readonly unknownPriorCycle: boolean;
}

export interface FirstDisplayGptDiagnosticsHandoffV1 extends FirstDisplayGptDiagnosticsV1 {
  readonly cycles: readonly Readonly<FirstDisplayGptDiagnosticCycleV1>[];
  readonly nextTraceTokenOrdinal: number;
}

export interface FirstDisplayGoogletagBatchCallbacks {
  readonly onBound: (cycle: FirstDisplayGptBoundCycleV1) => void;
  readonly onFailure: (slotId: string, reason: FirstDisplayGptFailureReason) => void;
  readonly onFirstAction: () => boolean;
  readonly onRenderEnded: (
    cycle: FirstDisplayGptBoundCycleV1,
    result: FirstDisplayGptRenderResult
  ) => void;
  /** Retire an accepted TS artifact when its exact parser-time physical binding is lost. */
  readonly onRetire?: (cycle: FirstDisplayGptBoundCycleV1) => void;
}

export interface FirstDisplayGoogletagBatch {
  readonly start: (callbacks: FirstDisplayGoogletagBatchCallbacks) => boolean;
  /** Stop provisional GPT ingress after every physical cycle is terminal. */
  /** Destroy noncommitted TS slots before closing publisher-mutation observation. */
  readonly closeIngress: (committedSlotIds: readonly string[]) => boolean;
  /** Capture exact terminal physical identities without changing disposal ownership. */
  readonly captureHandoff: () => readonly FirstDisplayGptHandoffCycleV1[] | undefined;
  readonly captureDiagnosticsHandoff: () => FirstDisplayGptDiagnosticsHandoffV1 | undefined;
  /** Exempt exactly the accepted slot identities from provisional destruction/restoration. */
  readonly detachCommittedSlots: (slotIds: readonly string[]) => boolean;
  readonly dispose: () => void;
}

export interface FirstDisplayGoogletagBatchOptions {
  readonly browser: Window & { googletag?: unknown };
  readonly clearTimer: (handle: unknown) => void;
  readonly document: Document;
  readonly diagnosticsActive?: boolean;
  readonly onNativeMutation?: () => boolean;
  readonly projection: FirstDisplayProjectionV1;
  readonly protocol: Pick<
    FirstDisplayGptProtocolV1,
    'classifyRenderEnded' | 'deadlines' | 'requestPlan'
  >;
  readonly setTimer: (callback: () => void, delayMs: number) => unknown;
}

export type FirstDisplayGoogletagBatchInput = Omit<FirstDisplayGoogletagBatchOptions, 'protocol'>;

type ExternalObject = Record<PropertyKey, unknown>;

interface FirstDisplayDiagnosticCycleRecord {
  readonly ordinal: number;
  readonly seen: Set<FirstDisplayGptDiagnosticEventV1>;
  responseIdentifier?: string;
  state: 'open' | 'completed' | 'retired';
}

interface ActiveCycle extends FirstDisplayGptBoundCycleV1 {
  readonly bindingState: { current: boolean };
  readonly diagnosticRecords: FirstDisplayDiagnosticCycleRecord[];
  readonly elementId: string;
  readonly operations: readonly ('display' | 'refresh')[];
  readonly requestOperation: 0 | 1;
  readonly runtimeSlotNumber: number;
  completionTimer?: unknown;
  nextDiagnosticCycleOrdinal: number;
  requestInvoked: boolean;
  requested: boolean;
  requestTimer?: unknown;
  settled: boolean;
  unknownPriorCycle: boolean;
}

interface TargetingRestorer {
  readonly installed: string;
  readonly key: string;
  readonly prior: readonly string[];
  readonly slot: object;
  valid: boolean;
}

interface TargetingWrite {
  consumed: boolean;
  readonly operation: 'clearTargeting' | 'setTargeting';
  readonly slot: object;
}

interface TargetingObserverRestorer {
  readonly isCurrent: () => boolean;
  readonly restore: () => boolean;
}

interface TargetingObserver {
  readonly isCurrent: () => boolean;
  readonly restore: () => boolean;
}

function externalObject(value: unknown): ExternalObject | undefined {
  return (typeof value === 'object' && value !== null) || typeof value === 'function'
    ? (value as ExternalObject)
    : undefined;
}

function member(value: unknown, key: PropertyKey): unknown {
  const object = externalObject(value);
  if (!object) return undefined;
  try {
    return Reflect.get(object, key);
  } catch {
    return undefined;
  }
}

function call(receiver: unknown, key: PropertyKey, arguments_: readonly unknown[]): unknown {
  const callable = member(receiver, key);
  if (typeof callable !== 'function') throw new TypeError('tsjs');
  return Reflect.apply(callable, receiver, arguments_);
}

function physicalSlot(value: unknown): object | undefined {
  return externalObject(value);
}

function resolveElement(
  document: Document,
  placement: FirstDisplayProjectionSlotV1
): HTMLElement | undefined {
  try {
    const ElementConstructor = document.defaultView?.HTMLElement;
    if (!ElementConstructor) return undefined;
    const exact = document.getElementById(placement.divId);
    if (exact instanceof ElementConstructor) return exact;
    const matches = [...document.querySelectorAll<HTMLElement>('[id]')].filter(
      (element) => element.id.startsWith(placement.divId) && !element.id.endsWith('-container')
    );
    return matches.length === 1 ? matches[0] : undefined;
  } catch {
    return undefined;
  }
}

function initialLoadDisabled(binding: ExternalObject): boolean {
  try {
    const getConfig = member(binding, 'getConfig');
    if (typeof getConfig !== 'function') return false;
    const config = Reflect.apply(getConfig, binding, ['disableInitialLoad']);
    return member(config, 'disableInitialLoad') === true;
  } catch {
    return false;
  }
}

function targetingEntries(
  bid: FirstDisplayProjectionBidV1,
  placement: FirstDisplayProjectionSlotV1
): readonly (readonly [string, string])[] {
  const targeting: Record<string, string> = {};
  for (const key of Object.keys(placement.targeting)) targeting[key] = placement.targeting[key]!;
  for (const key of Object.keys(bid.targeting)) targeting[key] = bid.targeting[key]!;
  targeting.hb_adid = bid.rendererReservationId;
  return Object.freeze(
    Object.keys(targeting)
      .sort()
      .map((key) => Object.freeze([key, targeting[key]!] as const))
  );
}

function winnerRows(projection: FirstDisplayProjectionV1): readonly Readonly<{
  bid: FirstDisplayProjectionBidV1;
  placement: FirstDisplayProjectionSlotV1;
}>[] {
  const rows: Array<
    Readonly<{ bid: FirstDisplayProjectionBidV1; placement: FirstDisplayProjectionSlotV1 }>
  > = [];
  let winner = 0;
  for (let index = 0; index < projection.auction.results.length; index += 1) {
    const decision = projection.auction.results[index];
    const placement = projection.slots[index];
    if (!decision || !placement || decision.outcome !== 'winner') continue;
    const bid = projection.bids[winner];
    winner += 1;
    if (bid) rows.push(Object.freeze({ bid, placement }));
  }
  return Object.freeze(rows);
}

class FirstDisplayGoogletagBatchOwner implements FirstDisplayGoogletagBatch {
  private readonly cycleMap = new Map<object, ActiveCycle>();
  private readonly createdSlots = new Set<object>();
  private readonly diagnosticFacts: Readonly<FirstDisplayGptFactV1>[] = [];
  private readonly diagnosticListeners = new Map<string, (event: unknown) => void>();
  private readonly targetingObservers = new Map<object, TargetingObserver>();
  private readonly targetingRestorers: TargetingRestorer[] = [];
  private readonly sealedTargetingOwnership = new Map<
    object,
    readonly FirstDisplayTargetingOwnershipV1[]
  >();
  private readonly publisherCallRestorers: Array<() => void> = [];
  private readonly timers = new Set<unknown>();
  private binding: ExternalObject | undefined;
  private command: (() => void) | undefined;
  private commandQueue: unknown[] | undefined;
  private commandQueueIndex = -1;
  private createdBinding: ExternalObject | undefined;
  private renderListener: ((event: unknown) => void) | undefined;
  private requestedListener: ((event: unknown) => void) | undefined;
  private service: ExternalObject | undefined;
  private started = false;
  private disposed = false;
  private ingressClosed = false;
  private ingressClosing = false;
  private committedSlotsDetached = false;
  private readonly detachedSlots = new Set<object>();
  private firstAction = false;
  private diagnosticFactOverflow = 0;
  private diagnosticFactDrops = 0;
  private nextTraceTokenOrdinal = 1;
  private readonly targetingWrites: TargetingWrite[] = [];

  public constructor(private readonly options: FirstDisplayGoogletagBatchOptions) {}

  public start(callbacks: FirstDisplayGoogletagBatchCallbacks): boolean {
    if (this.started || this.disposed) return false;
    this.started = true;
    const rows = winnerRows(this.options.projection);
    if (rows.length === 0) return true;

    const binding = this.ensureBinding();
    const queue = member(binding, 'cmd');
    const push = member(queue, 'push');
    if (!binding || !queue || typeof push !== 'function') {
      this.failRows(rows, callbacks, 'external_artifact_incompatible');
      return false;
    }
    this.binding = binding;
    const readyTimer = this.timer(() => {
      this.removePendingCommand();
      this.failRows(rows, callbacks, 'external_ready_timeout');
    }, this.options.protocol.deadlines.externalReadyMs);
    const command = (): void => {
      if (this.disposed || this.command !== command) return;
      this.command = undefined;
      this.clearOwnedTimer(readyTimer);
      try {
        this.activate(binding, rows, callbacks);
      } catch {
        this.failRows(rows, callbacks, 'external_artifact_incompatible');
      }
    };
    this.command = command;
    if (Array.isArray(queue)) {
      this.commandQueue = queue;
      this.commandQueueIndex = queue.length;
    }
    try {
      Reflect.apply(push, queue, [command]);
    } catch {
      this.command = undefined;
      this.clearOwnedTimer(readyTimer);
      this.failRows(rows, callbacks, 'external_artifact_incompatible');
      return false;
    }
    return true;
  }

  public closeIngress(committedSlotIds: readonly string[]): boolean {
    if (
      this.disposed ||
      this.ingressClosed ||
      this.ingressClosing ||
      !this.started ||
      [...this.cycleMap.values()].some((cycle) => !cycle.settled) ||
      !Array.isArray(committedSlotIds)
    ) {
      return false;
    }
    const committed = new Set(committedSlotIds);
    if (committed.size !== committedSlotIds.length) return false;
    const retainedSlots = new Set<object>();
    for (const slotId of committed) {
      const matches = [...this.cycleMap.values()].filter((cycle) => cycle.slotId === slotId);
      if (matches.length !== 1 || !matches[0]?.settled) return false;
      retainedSlots.add(matches[0].physicalSlot);
    }
    const destroyableSlots = [...this.createdSlots].filter((slot) => !retainedSlots.has(slot));
    this.ingressClosing = true;
    if (destroyableSlots.length > 0) {
      try {
        if (!this.binding || call(this.binding, 'destroySlots', [[...destroyableSlots]]) !== true) {
          this.ingressClosing = false;
          return false;
        }
      } catch {
        this.ingressClosing = false;
        return false;
      }
      for (const slot of destroyableSlots) this.createdSlots.delete(slot);
    }
    this.invalidateStaleTargetingObservers();
    const retainedTargetingRestorers = this.targetingRestorers.filter((restoration) =>
      retainedSlots.has(restoration.slot)
    );
    for (const restoration of [...this.targetingRestorers].reverse()) {
      if (retainedSlots.has(restoration.slot)) continue;
      try {
        this.restorePublisherTargeting(restoration);
      } catch {
        // Publisher mutations win while retained-slot observation is still authoritative.
      }
    }
    this.targetingRestorers.splice(
      0,
      this.targetingRestorers.length,
      ...retainedTargetingRestorers
    );
    this.ingressClosed = true;
    this.ingressClosing = false;
    this.removePendingCommand();
    for (const handle of [...this.timers]) this.clearOwnedTimer(handle);
    this.removeListener();
    this.restorePublisherCalls();
    const targetingCandidates = this.captureRetainedTargeting(retainedSlots);
    this.restoreTargetingObservers();
    for (const [slot, restorations] of targetingCandidates) {
      this.sealedTargetingOwnership.set(
        slot,
        Object.freeze(
          restorations
            .filter(({ valid }) => valid)
            .map(({ installed, key, prior }) => Object.freeze({ installed, key, prior }))
        )
      );
    }
    return true;
  }

  public captureHandoff(): readonly FirstDisplayGptHandoffCycleV1[] | undefined {
    if (this.disposed || !this.ingressClosed) return undefined;
    return Object.freeze(
      [...this.cycleMap.values()].map((cycle) => {
        const targetingOwnership =
          this.sealedTargetingOwnership.get(cycle.physicalSlot) ?? Object.freeze([]);
        return Object.freeze({
          bid: cycle.bid,
          element: cycle.element,
          isCurrent: cycle.isCurrent,
          ownership: cycle.ownership,
          physicalSlot: cycle.physicalSlot,
          placement: cycle.placement,
          slotId: cycle.slotId,
          targetingOwnership: Object.freeze(targetingOwnership),
          traceToken: cycle.traceToken,
        });
      })
    );
  }

  public captureDiagnosticsHandoff(): FirstDisplayGptDiagnosticsHandoffV1 | undefined {
    if (this.disposed || !this.ingressClosed) return undefined;
    return Object.freeze({
      cycles: Object.freeze(
        [...this.cycleMap.values()].map((cycle) =>
          Object.freeze({
            nextCycleOrdinal: cycle.nextDiagnosticCycleOrdinal,
            quarantines: Object.freeze([]),
            records: Object.freeze(
              cycle.diagnosticRecords.map((record) =>
                Object.freeze({
                  ordinal: record.ordinal,
                  responseIdentifier: record.responseIdentifier ?? null,
                  seen: Object.freeze(
                    DIAGNOSTIC_EVENT_ORDER.filter((event) => record.seen.has(event))
                  ),
                  state: record.state,
                })
              )
            ),
            slotId: cycle.slotId,
            token: cycle.traceToken,
            unknownPriorCycle: cycle.unknownPriorCycle,
          })
        )
      ),
      facts: Object.freeze([...this.diagnosticFacts]),
      dropCount: this.diagnosticFactDrops,
      nextTraceTokenOrdinal: this.nextTraceTokenOrdinal,
      overflowCount: this.diagnosticFactOverflow,
    });
  }

  public detachCommittedSlots(slotIds: readonly string[]): boolean {
    if (this.disposed || !this.ingressClosed || this.committedSlotsDetached) return false;
    const requested = new Set(slotIds);
    if (requested.size !== slotIds.length) return false;
    const selected: object[] = [];
    for (const slotId of requested) {
      const matches = [...this.cycleMap.values()].filter((cycle) => cycle.slotId === slotId);
      if (matches.length !== 1 || !matches[0]?.settled) return false;
      selected.push(matches[0].physicalSlot);
    }
    this.committedSlotsDetached = true;
    for (const slot of selected) this.detachedSlots.add(slot);
    return true;
  }

  public dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.removePendingCommand();
    for (const handle of [...this.timers]) this.clearOwnedTimer(handle);
    this.removeListener();
    this.restorePublisherCalls();
    this.invalidateStaleTargetingObservers();
    for (const restoration of this.targetingRestorers.reverse()) {
      if (this.detachedSlots.has(restoration.slot)) continue;
      try {
        this.restorePublisherTargeting(restoration);
      } catch {
        // Publisher mutations and hostile GPT objects always win over restoration.
      }
    }
    this.targetingRestorers.length = 0;
    this.restoreTargetingObservers();
    const destroyableSlots = [...this.createdSlots].filter((slot) => !this.detachedSlots.has(slot));
    if (this.binding && destroyableSlots.length > 0) {
      try {
        call(this.binding, 'destroySlots', [destroyableSlots]);
      } catch {
        // Generation latching keeps failed physical cleanup inert.
      }
    }
    this.createdSlots.clear();
    this.cycleMap.clear();
    this.sealedTargetingOwnership.clear();
    if (this.detachedSlots.size === 0) this.restoreCreatedBinding();
    else this.createdBinding = undefined;
    this.detachedSlots.clear();
    this.binding = undefined;
    this.service = undefined;
  }

  private ensureBinding(): ExternalObject | undefined {
    const current = externalObject(this.options.browser.googletag);
    if (current) return current;
    const binding: ExternalObject = { cmd: [] };
    try {
      if (
        !Reflect.defineProperty(this.options.browser, 'googletag', {
          configurable: true,
          enumerable: true,
          value: binding,
          writable: true,
        }) ||
        this.options.browser.googletag !== binding
      ) {
        return undefined;
      }
      this.createdBinding = binding;
      return binding;
    } catch {
      return undefined;
    }
  }

  private restoreCreatedBinding(): void {
    if (!this.createdBinding) return;
    try {
      const descriptor = Object.getOwnPropertyDescriptor(this.options.browser, 'googletag');
      if (descriptor && 'value' in descriptor && descriptor.value === this.createdBinding) {
        Reflect.deleteProperty(this.options.browser, 'googletag');
      }
    } catch {
      // Publisher replacement wins over restoration.
    }
    this.createdBinding = undefined;
  }

  private activate(
    binding: ExternalObject,
    rows: readonly Readonly<{
      bid: FirstDisplayProjectionBidV1;
      placement: FirstDisplayProjectionSlotV1;
    }>[],
    callbacks: FirstDisplayGoogletagBatchCallbacks
  ): void {
    if (this.disposed || this.binding !== binding) return;
    const service = externalObject(call(binding, 'pubads', []));
    if (!service) throw new TypeError('tsjs');
    this.service = service;
    this.observePublisherCalls(binding, service, callbacks);
    this.installListeners(service, callbacks);
    const existing = call(service, 'getSlots', []);
    if (!Array.isArray(existing)) throw new TypeError('tsjs');
    const disabled = initialLoadDisabled(binding);

    for (const row of rows) {
      if (this.disposed) return;
      const element = resolveElement(this.options.document, row.placement);
      if (!element) {
        callbacks.onFailure(row.placement.slot, SLOT_UNRESOLVED);
        continue;
      }
      const matches = existing.filter((candidate) =>
        physicalSlot(candidate) && typeof member(candidate, 'getSlotElementId') === 'function'
          ? call(candidate, 'getSlotElementId', []) === element.id
          : member(candidate, 'getSlotElementId') === undefined
            ? member(candidate, 'elementId') === element.id
            : false
      );
      if (matches.length > 1) {
        callbacks.onFailure(row.placement.slot, SLOT_UNRESOLVED);
        continue;
      }
      const publisherSlot = physicalSlot(matches[0]);
      const slot =
        publisherSlot ??
        physicalSlot(
          call(binding, 'defineSlot', [
            row.placement.gamUnitPath,
            row.placement.formats,
            element.id,
          ])
        );
      if (!slot) {
        callbacks.onFailure(row.placement.slot, SLOT_UNRESOLVED);
        continue;
      }
      const ownership = publisherSlot ? 'publisher' : 'trusted_server';
      if (!publisherSlot) {
        call(slot, 'addService', [service]);
        this.createdSlots.add(slot);
      } else {
        this.observePublisherTargeting(slot);
      }
      const plan = this.options.protocol.requestPlan(
        Object.freeze({ initialLoadDisabled: disabled, ownership })
      );
      if (!plan) {
        callbacks.onFailure(row.placement.slot, GPT_REQUEST_FAILED);
        continue;
      }
      const traceTokenOrdinal = this.nextTraceTokenOrdinal;
      if (traceTokenOrdinal > 4_294_967_295) {
        callbacks.onFailure(row.placement.slot, GPT_REQUEST_FAILED);
        continue;
      }
      const traceToken = `gt1_${traceTokenOrdinal.toString(36)}`;
      this.nextTraceTokenOrdinal += 1;
      const bindingState = { current: true };
      const cycle: ActiveCycle = {
        bid: row.bid,
        bindingState,
        diagnosticRecords: [],
        element,
        elementId: element.id,
        isCurrent: () => bindingState.current,
        operations: plan.operations,
        ownership,
        physicalSlot: slot,
        placement: row.placement,
        requestOperation: plan.requestOperation,
        runtimeSlotNumber: traceTokenOrdinal,
        nextDiagnosticCycleOrdinal: 1,
        requestInvoked: false,
        requested: false,
        settled: false,
        slotId: row.placement.slot,
        traceToken,
        unknownPriorCycle: ownership === 'publisher',
      };
      this.cycleMap.set(slot, cycle);
      callbacks.onBound(
        Object.freeze({
          bid: cycle.bid,
          element: cycle.element,
          isCurrent: cycle.isCurrent,
          ownership: cycle.ownership,
          physicalSlot: cycle.physicalSlot,
          placement: cycle.placement,
          slotId: cycle.slotId,
          traceToken: cycle.traceToken,
        })
      );
      for (const [key, value] of targetingEntries(row.bid, row.placement)) {
        if (publisherSlot) this.journalPublisherTargeting(slot, key, value);
        this.writeTargeting(slot, 'setTargeting', [key, value]);
      }
    }

    for (const cycle of this.cycleMap.values()) {
      if (this.disposed || cycle.settled) continue;
      try {
        for (let index = 0; index < cycle.operations.length; index += 1) {
          const operation = cycle.operations[index];
          if (index === cycle.requestOperation) {
            cycle.requestInvoked = true;
            cycle.requestTimer = this.timer(
              () => this.failCycle(cycle, callbacks, 'gpt_request_timeout'),
              this.options.protocol.deadlines.requestStartMs
            );
            cycle.completionTimer = this.timer(
              () => this.failCycle(cycle, callbacks, 'gpt_completion_timeout'),
              this.options.protocol.deadlines.completionMs
            );
            if (!this.firstAction) {
              this.firstAction = true;
              if (!callbacks.onFirstAction()) throw new TypeError('tsjs');
            }
          }
          if (operation === 'display') call(binding, 'display', [cycle.elementId]);
          else call(service, 'refresh', [[cycle.physicalSlot], { changeCorrelator: false }]);
        }
      } catch {
        if (cycle.settled) continue;
        this.failCycle(cycle, callbacks, GPT_REQUEST_FAILED);
      }
    }
  }

  private installListeners(
    service: ExternalObject,
    callbacks: FirstDisplayGoogletagBatchCallbacks
  ): void {
    const requestedListener = (event: unknown): void => {
      if (this.disposed || this.requestedListener !== requestedListener) return;
      this.notifyNativeMutation();
      const slot = physicalSlot(member(event, 'slot'));
      const cycle = slot ? this.cycleMap.get(slot) : undefined;
      if (!cycle) {
        if (slot) {
          this.invalidateCyclesForElement(this.readPhysicalElementId(slot), slot, callbacks);
        }
        return;
      }
      if (cycle.settled) {
        this.retireCycle(cycle, callbacks);
        this.captureDiagnosticFact(SLOT_REQUESTED, cycle, event);
        return;
      }
      if (!cycle.requestInvoked) return;
      if (cycle.requested) {
        this.failCycle(cycle, callbacks, 'cycle_unattributable');
        return;
      }
      cycle.requested = true;
      this.captureDiagnosticFact(SLOT_REQUESTED, cycle, event);
      if (cycle.requestTimer !== undefined) this.clearOwnedTimer(cycle.requestTimer);
    };
    const renderListener = (event: unknown): void => {
      if (this.disposed || this.renderListener !== renderListener) return;
      this.notifyNativeMutation();
      const slot = physicalSlot(member(event, 'slot'));
      const cycle = slot ? this.cycleMap.get(slot) : undefined;
      if (!cycle) return;
      if (cycle.settled) {
        this.captureDiagnosticFact(SLOT_RENDER_ENDED, cycle, event);
        return;
      }
      if (!cycle.requested) return;
      const result = this.options.protocol.classifyRenderEnded(
        Object.freeze({ isEmpty: member(event, 'isEmpty') })
      );
      if (!result) {
        this.failCycle(cycle, callbacks, GPT_REQUEST_FAILED);
        return;
      }
      this.captureDiagnosticFact(SLOT_RENDER_ENDED, cycle, event);
      cycle.settled = true;
      if (cycle.requestTimer !== undefined) this.clearOwnedTimer(cycle.requestTimer);
      if (cycle.completionTimer !== undefined) this.clearOwnedTimer(cycle.completionTimer);
      callbacks.onRenderEnded(
        Object.freeze({
          bid: cycle.bid,
          element: cycle.element,
          isCurrent: cycle.isCurrent,
          ownership: cycle.ownership,
          physicalSlot: cycle.physicalSlot,
          placement: cycle.placement,
          slotId: cycle.slotId,
          traceToken: cycle.traceToken,
        }),
        result
      );
    };
    this.requestedListener = requestedListener;
    this.renderListener = renderListener;
    this.diagnosticListeners.set(SLOT_REQUESTED, requestedListener);
    this.diagnosticListeners.set(SLOT_RENDER_ENDED, renderListener);
    call(service, 'addEventListener', [SLOT_REQUESTED, requestedListener]);
    call(service, 'addEventListener', [SLOT_RENDER_ENDED, renderListener]);
    if (this.options.diagnosticsActive === true) {
      for (const eventType of DIAGNOSTIC_ONLY_EVENTS) {
        const listener = (event: unknown): void => {
          if (this.disposed || this.diagnosticListeners.get(eventType) !== listener) return;
          this.notifyNativeMutation();
          const slot = physicalSlot(member(event, 'slot'));
          const cycle = slot ? this.cycleMap.get(slot) : undefined;
          if (cycle) this.captureDiagnosticFact(eventType, cycle, event);
        };
        this.diagnosticListeners.set(eventType, listener);
        call(service, 'addEventListener', [eventType, listener]);
      }
    }
  }

  private removeListener(): void {
    if (!this.service) return;
    this.requestedListener = undefined;
    this.renderListener = undefined;
    try {
      for (const [eventType, listener] of this.diagnosticListeners) {
        call(this.service, 'removeEventListener', [eventType, listener]);
      }
    } catch {
      // The generation latch remains authoritative if GPT cannot detach physically.
    } finally {
      this.diagnosticListeners.clear();
    }
  }

  private failCycle(
    cycle: ActiveCycle,
    callbacks: FirstDisplayGoogletagBatchCallbacks,
    reason: FirstDisplayGptFailureReason
  ): void {
    if (cycle.settled) return;
    cycle.settled = true;
    for (const record of cycle.diagnosticRecords) {
      if (record.state === 'open') record.state = 'retired';
    }
    if (cycle.requestTimer !== undefined) this.clearOwnedTimer(cycle.requestTimer);
    if (cycle.completionTimer !== undefined) this.clearOwnedTimer(cycle.completionTimer);
    callbacks.onFailure(cycle.slotId, reason);
  }

  private journalPublisherTargeting(slot: object, key: string, installed: string): void {
    const original = call(slot, 'getTargeting', [key]);
    if (!Array.isArray(original) || !original.every((value) => typeof value === 'string')) {
      throw new TypeError('tsjs');
    }
    this.targetingRestorers.push({
      installed,
      key,
      prior: Object.freeze([...original]),
      slot,
      valid: true,
    });
  }

  private observePublisherTargeting(slot: object): void {
    if (this.targetingObservers.has(slot)) return;
    const restorers: TargetingObserverRestorer[] = [];
    try {
      for (const key of ['setTargeting', 'clearTargeting'] as const) {
        const original = member(slot, key);
        if (typeof original !== 'function') throw new TypeError('tsjs');
        const prior = Object.getOwnPropertyDescriptor(slot, key);
        const invalidateTargeting = (targetingKey: string | undefined): void =>
          this.invalidateTargeting(slot, targetingKey);
        const consumeTrustedServerWrite = (): boolean => this.consumeTargetingWrite(slot, key);
        const ownerMutation = (): void => this.notifyNativeMutation();
        const wrapper = function (this: unknown, ...arguments_: unknown[]): unknown {
          const trustedServerWrite = this === slot && consumeTrustedServerWrite();
          if (!trustedServerWrite && this === slot) {
            const targetingKey = typeof arguments_[0] === 'string' ? arguments_[0] : undefined;
            invalidateTargeting(targetingKey);
            const result = Reflect.apply(original, this, arguments_);
            ownerMutation();
            return result;
          }
          return Reflect.apply(original, this, arguments_);
        };
        if (
          !Reflect.defineProperty(slot, key, {
            configurable: true,
            enumerable: prior?.enumerable ?? false,
            value: wrapper,
            writable: true,
          })
        ) {
          throw new TypeError('tsjs');
        }
        const isCurrent = (): boolean => {
          try {
            const current = Object.getOwnPropertyDescriptor(slot, key);
            return !!current && 'value' in current && current.value === wrapper;
          } catch {
            return false;
          }
        };
        restorers.push({
          isCurrent,
          restore: (): boolean => {
            if (!isCurrent()) return false;
            try {
              return prior
                ? Reflect.defineProperty(slot, key, prior)
                : Reflect.deleteProperty(slot, key);
            } catch {
              return false;
            }
          },
        });
      }
    } catch (error) {
      for (const restorer of restorers.reverse()) restorer.restore();
      throw error;
    }
    this.targetingObservers.set(slot, {
      isCurrent: (): boolean => restorers.every(({ isCurrent }) => isCurrent()),
      restore: (): boolean => {
        let exact = restorers.every(({ isCurrent }) => isCurrent());
        for (const restorer of restorers.reverse()) {
          if (!restorer.restore()) exact = false;
        }
        return exact;
      },
    });
  }

  private observePublisherCalls(
    binding: ExternalObject,
    service: ExternalObject,
    callbacks: FirstDisplayGoogletagBatchCallbacks
  ): void {
    const installed: Array<() => void> = [];
    try {
      for (const [receiver, key] of [
        [binding, 'defineSlot'],
        [binding, 'destroySlots'],
        [binding, 'display'],
        [service, 'refresh'],
      ] as const) {
        const original = member(receiver, key);
        if (typeof original !== 'function') continue;
        const prior = Object.getOwnPropertyDescriptor(receiver, key);
        const notify = (): void => this.notifyNativeMutation();
        const snapshotDestroy = (arguments_: readonly unknown[]): readonly ActiveCycle[] =>
          key === 'destroySlots' ? this.snapshotDestroyedCycles(arguments_) : Object.freeze([]);
        const invalidate = (
          arguments_: readonly unknown[],
          result: unknown,
          destroyed: readonly ActiveCycle[]
        ): void =>
          this.invalidateCyclesForPublisherCall(key, arguments_, result, destroyed, callbacks);
        const wrapper = function (this: unknown, ...arguments_: unknown[]): unknown {
          const destroyed = this === receiver ? snapshotDestroy(arguments_) : Object.freeze([]);
          const result = Reflect.apply(original, this, arguments_);
          if (this === receiver) {
            invalidate(arguments_, result, destroyed);
            notify();
          }
          return result;
        };
        if (
          !Reflect.defineProperty(receiver, key, {
            configurable: true,
            enumerable: prior?.enumerable ?? false,
            value: wrapper,
            writable: true,
          })
        ) {
          throw new TypeError('tsjs');
        }
        installed.push(() => {
          const current = Object.getOwnPropertyDescriptor(receiver, key);
          if (!current || !('value' in current) || current.value !== wrapper) return;
          if (prior) Reflect.defineProperty(receiver, key, prior);
          else Reflect.deleteProperty(receiver, key);
        });
      }
    } catch (error) {
      for (const restore of installed.reverse()) restore();
      throw error;
    }
    this.publisherCallRestorers.push(...installed);
  }

  private readPhysicalElementId(slot: object): string | undefined {
    try {
      const read = member(slot, 'getSlotElementId');
      if (typeof read !== 'function') return undefined;
      const value = Reflect.apply(read, slot, []);
      return typeof value === 'string' && value.length > 0 ? value : undefined;
    } catch {
      return undefined;
    }
  }

  private retireCycle(cycle: ActiveCycle, callbacks: FirstDisplayGoogletagBatchCallbacks): boolean {
    if (!cycle.bindingState.current) return false;
    cycle.bindingState.current = false;
    try {
      callbacks.onRetire?.(
        Object.freeze({
          bid: cycle.bid,
          element: cycle.element,
          isCurrent: cycle.isCurrent,
          ownership: cycle.ownership,
          physicalSlot: cycle.physicalSlot,
          placement: cycle.placement,
          slotId: cycle.slotId,
          traceToken: cycle.traceToken,
        })
      );
    } catch {
      // Binding invalidation remains authoritative when retirement observation fails.
    }
    return true;
  }

  private snapshotDestroyedCycles(arguments_: readonly unknown[]): readonly ActiveCycle[] {
    try {
      const targets = arguments_[0];
      if (targets === undefined) return Object.freeze([...this.cycleMap.values()]);
      if (!Array.isArray(targets)) return Object.freeze([]);
      const cycles = new Set<ActiveCycle>();
      for (let index = 0; index < targets.length; index += 1) {
        const target = physicalSlot(targets[index]);
        const cycle = target ? this.cycleMap.get(target) : undefined;
        if (cycle) cycles.add(cycle);
      }
      return Object.freeze([...cycles]);
    } catch {
      return Object.freeze([]);
    }
  }

  private invalidateCyclesForElement(
    elementId: string | undefined,
    replacement: object,
    callbacks: FirstDisplayGoogletagBatchCallbacks
  ): void {
    if (!elementId) return;
    for (const cycle of this.cycleMap.values()) {
      if (cycle.physicalSlot !== replacement && cycle.elementId === elementId) {
        this.retireCycle(cycle, callbacks);
      }
    }
  }

  private invalidateCyclesForPublisherCall(
    key: string,
    arguments_: readonly unknown[],
    result: unknown,
    destroyed: readonly ActiveCycle[],
    callbacks: FirstDisplayGoogletagBatchCallbacks
  ): void {
    try {
      if (key === 'destroySlots' && result === true) {
        for (const cycle of destroyed) this.retireCycle(cycle, callbacks);
        return;
      }
      if (key === 'defineSlot') {
        const replacement = physicalSlot(result);
        const elementId = arguments_[2];
        if (replacement && typeof elementId === 'string') {
          this.invalidateCyclesForElement(elementId, replacement, callbacks);
        }
      }
    } catch {
      // The publisher call already completed; observation cannot alter its result.
    }
  }

  private restorePublisherCalls(): void {
    for (const restore of this.publisherCallRestorers.reverse()) {
      try {
        restore();
      } catch {
        // Publisher replacement wins while the old wrapper loses all authority.
      }
    }
    this.publisherCallRestorers.length = 0;
  }

  private notifyNativeMutation(): void {
    try {
      this.options.onNativeMutation?.();
    } catch {
      // Mutation observation failure cannot alter the publisher's admitted call.
    }
  }

  private captureDiagnosticFact(
    eventType: FirstDisplayGptDiagnosticEventV1,
    cycle: ActiveCycle,
    event: unknown
  ): void {
    const responseIdentifier = (() => {
      const candidate = member(event, 'responseIdentifier');
      if (typeof candidate !== 'string' || candidate.length === 0) return undefined;
      if (new TextEncoder().encode(candidate).byteLength > 256) return undefined;
      for (let index = 0; index < candidate.length; index += 1) {
        const code = candidate.charCodeAt(index);
        if (code <= 0x1f || code === 0x7f) return undefined;
      }
      return candidate;
    })();
    let disposition: FirstDisplayGptFactV1['disposition'] = 'matched';
    let issueReason: FirstDisplayGptFactV1['issueReason'] = null;
    let diagnosticCycle: FirstDisplayDiagnosticCycleRecord | undefined;
    if (eventType === SLOT_REQUESTED) {
      if (cycle.diagnosticRecords.some((record) => record.state === 'open')) {
        disposition = 'ambiguous';
        issueReason = OVERLAPPING_REQUEST_CYCLES;
      } else if (cycle.nextDiagnosticCycleOrdinal > MAX_U32) {
        disposition = 'unmatched';
        issueReason = INVALID_EVENT_ORDER;
      } else {
        if (cycle.diagnosticRecords.length >= 10) {
          const pruneIndex = cycle.diagnosticRecords.findIndex((record) => record.state !== 'open');
          if (pruneIndex < 0) {
            disposition = 'ambiguous';
            issueReason = OVERLAPPING_REQUEST_CYCLES;
          } else {
            cycle.diagnosticRecords.splice(pruneIndex, 1);
            cycle.unknownPriorCycle = true;
          }
        }
        if (disposition === 'matched') {
          diagnosticCycle = {
            ordinal: cycle.nextDiagnosticCycleOrdinal,
            ...(responseIdentifier === undefined ? {} : { responseIdentifier }),
            seen: new Set([SLOT_REQUESTED]),
            state: 'open',
          };
          cycle.nextDiagnosticCycleOrdinal += 1;
          cycle.diagnosticRecords.push(diagnosticCycle);
        }
      }
    } else {
      let candidates: FirstDisplayDiagnosticCycleRecord[];
      if (responseIdentifier !== undefined) {
        candidates = cycle.diagnosticRecords.filter(
          (record) =>
            record.responseIdentifier === responseIdentifier && !record.seen.has(eventType)
        );
        if (candidates.length === 0) {
          const unboundOpen = cycle.diagnosticRecords.filter(
            (record) =>
              record.state === 'open' &&
              record.responseIdentifier === undefined &&
              !record.seen.has(eventType)
          );
          if (unboundOpen.length === 1) candidates = unboundOpen;
        }
      } else {
        candidates = cycle.diagnosticRecords.filter(
          (record) => record.state === 'open' && !record.seen.has(eventType)
        );
        if (candidates.length === 0 && !cycle.unknownPriorCycle) {
          candidates = cycle.diagnosticRecords.filter((record) => !record.seen.has(eventType));
        }
      }
      if (candidates.length === 1) {
        diagnosticCycle = candidates[0];
      } else if (candidates.length > 1) {
        disposition = 'ambiguous';
        issueReason = OVERLAPPING_REQUEST_CYCLES;
      } else {
        const duplicate = cycle.diagnosticRecords.find(
          (record) =>
            record.seen.has(eventType) &&
            (responseIdentifier === undefined ||
              record.responseIdentifier === responseIdentifier ||
              record.responseIdentifier === undefined)
        );
        if (duplicate) {
          diagnosticCycle = duplicate;
          issueReason = INVALID_EVENT_ORDER;
        } else {
          disposition = 'unmatched';
          issueReason = cycle.unknownPriorCycle ? 'unknown_prior_cycle' : 'no_request_cycle';
        }
      }
      if (diagnosticCycle && issueReason !== INVALID_EVENT_ORDER) {
        diagnosticCycle.seen.add(eventType);
        if (diagnosticCycle.responseIdentifier === undefined && responseIdentifier !== undefined) {
          diagnosticCycle.responseIdentifier = responseIdentifier;
        }
        if (eventType === SLOT_RENDER_ENDED) diagnosticCycle.state = 'completed';
      }
    }
    if (this.options.diagnosticsActive !== true) return;
    let observedAtMs: number;
    try {
      observedAtMs = this.options.browser.performance.now();
      if (!Number.isFinite(observedAtMs) || observedAtMs < 0) {
        this.incrementDiagnosticDrops();
        return;
      }
    } catch {
      this.incrementDiagnosticDrops();
      return;
    }
    const size = member(event, 'size');
    const renderedSize =
      eventType === SLOT_RENDER_ENDED &&
      Array.isArray(size) &&
      size.length === 2 &&
      size.every(
        (dimension) =>
          typeof dimension === 'number' &&
          Number.isInteger(dimension) &&
          dimension >= 1 &&
          dimension <= 4096
      )
        ? Object.freeze([size[0] as number, size[1] as number] as const)
        : null;
    const optionalBoolean = (key: string): boolean | null => {
      const value = member(event, key);
      return typeof value === 'boolean' ? value : null;
    };
    const visibility = member(event, 'inViewPercentage');
    const fact: Readonly<FirstDisplayGptFactV1> = Object.freeze({
      version: 1,
      event: eventType,
      token: cycle.traceToken,
      runtimeSlotNumber: cycle.runtimeSlotNumber,
      cycleOrdinal: disposition === 'matched' ? (diagnosticCycle?.ordinal ?? null) : null,
      disposition,
      issueReason,
      capturedAtMs: observedAtMs,
      elementId: cycle.elementId,
      adUnitPath: cycle.placement.gamUnitPath,
      isEmpty: eventType === SLOT_RENDER_ENDED ? optionalBoolean('isEmpty') : null,
      renderedSize,
      isBackfill: eventType === SLOT_RENDER_ENDED ? optionalBoolean('isBackfill') : null,
      slotContentChanged:
        eventType === SLOT_RENDER_ENDED ? optionalBoolean('slotContentChanged') : null,
      visibilityPercent:
        eventType === SLOT_VISIBILITY_CHANGED &&
        typeof visibility === 'number' &&
        Number.isFinite(visibility) &&
        visibility >= 0 &&
        visibility <= 100
          ? visibility
          : null,
    });
    if (this.encodedBytes(fact) > MAX_DIAGNOSTIC_FACT_BYTES) {
      this.incrementDiagnosticDrops();
      return;
    }
    const overflow = this.diagnosticFacts.length >= MAX_DIAGNOSTIC_FACTS;
    const nextFacts = overflow
      ? [...this.diagnosticFacts.slice(1), fact]
      : [...this.diagnosticFacts, fact];
    const nextOverflow = overflow
      ? Math.min(MAX_U32, this.diagnosticFactOverflow + 1)
      : this.diagnosticFactOverflow;
    if (
      this.encodedBytes({
        facts: nextFacts,
        overflowCount: nextOverflow,
        dropCount: this.diagnosticFactDrops,
      }) > MAX_DIAGNOSTIC_SECTION_BYTES
    ) {
      this.incrementDiagnosticDrops();
      return;
    }
    this.diagnosticFacts.splice(0, this.diagnosticFacts.length, ...nextFacts);
    this.diagnosticFactOverflow = nextOverflow;
  }

  private encodedBytes(value: unknown): number {
    try {
      return new TextEncoder().encode(JSON.stringify(value)).byteLength;
    } catch {
      return Number.POSITIVE_INFINITY;
    }
  }

  private incrementDiagnosticDrops(): void {
    this.diagnosticFactDrops = Math.min(MAX_U32, this.diagnosticFactDrops + 1);
  }

  private invalidateTargeting(slot: object, key: string | undefined): void {
    for (const restoration of this.targetingRestorers) {
      if (restoration.slot === slot && (key === undefined || restoration.key === key)) {
        restoration.valid = false;
      }
    }
  }

  private restorePublisherTargeting(restoration: TargetingRestorer): void {
    if (!restoration.valid) return;
    const observer = this.targetingObservers.get(restoration.slot);
    if (observer && !observer.isCurrent()) {
      this.invalidateTargeting(restoration.slot, undefined);
      return;
    }
    if (!restoration.valid) return;
    const current = call(restoration.slot, 'getTargeting', [restoration.key]);
    if (!restoration.valid) return;
    if (observer && !observer.isCurrent()) {
      this.invalidateTargeting(restoration.slot, undefined);
      return;
    }
    if (
      !restoration.valid ||
      !Array.isArray(current) ||
      current.length !== 1 ||
      current[0] !== restoration.installed
    ) {
      return;
    }
    if (restoration.prior.length === 0) {
      this.writeTargeting(restoration.slot, 'clearTargeting', [restoration.key]);
    } else {
      this.writeTargeting(restoration.slot, 'setTargeting', [restoration.key, restoration.prior]);
    }
  }

  private writeTargeting(
    slot: object,
    operation: 'clearTargeting' | 'setTargeting',
    arguments_: readonly unknown[]
  ): unknown {
    const write: TargetingWrite = { consumed: false, operation, slot };
    this.targetingWrites.push(write);
    try {
      return call(slot, operation, arguments_);
    } finally {
      const index = this.targetingWrites.lastIndexOf(write);
      if (index >= 0) this.targetingWrites.splice(index, 1);
    }
  }

  private consumeTargetingWrite(
    slot: object,
    operation: 'clearTargeting' | 'setTargeting'
  ): boolean {
    const write = this.targetingWrites[this.targetingWrites.length - 1];
    if (!write || write.consumed || write.slot !== slot || write.operation !== operation) {
      return false;
    }
    write.consumed = true;
    return true;
  }

  private invalidateStaleTargetingObservers(): void {
    for (const [slot, observer] of this.targetingObservers) {
      if (!observer.isCurrent()) this.invalidateTargeting(slot, undefined);
    }
  }

  private restoreTargetingObservers(): void {
    for (const [slot, observer] of this.targetingObservers) {
      if (!observer.restore()) this.invalidateTargeting(slot, undefined);
    }
    this.targetingObservers.clear();
  }

  private captureRetainedTargeting(
    retainedSlots: ReadonlySet<object>
  ): Map<object, TargetingRestorer[]> {
    const captured = new Map<object, TargetingRestorer[]>();
    for (const slot of retainedSlots) {
      const observer = this.targetingObservers.get(slot);
      if (observer && !observer.isCurrent()) this.invalidateTargeting(slot, undefined);
      const restorations: TargetingRestorer[] = [];
      for (const restoration of this.targetingRestorers) {
        if (!restoration.valid || restoration.slot !== slot) continue;
        let current: unknown;
        try {
          current = call(slot, 'getTargeting', [restoration.key]);
        } catch {
          continue;
        }
        if (observer && !observer.isCurrent()) {
          this.invalidateTargeting(slot, undefined);
          break;
        }
        if (
          restoration.valid &&
          Array.isArray(current) &&
          current.length === 1 &&
          current[0] === restoration.installed
        ) {
          restorations.push(restoration);
        }
      }
      captured.set(slot, restorations);
    }
    return captured;
  }

  private failRows(
    rows: readonly Readonly<{ placement: FirstDisplayProjectionSlotV1 }>[],
    callbacks: FirstDisplayGoogletagBatchCallbacks,
    reason: FirstDisplayGptFailureReason
  ): void {
    for (const row of rows) {
      try {
        callbacks.onFailure(row.placement.slot, reason);
      } catch {
        // One consumer cannot prevent independent slot settlement.
      }
    }
  }

  private timer(callback: () => void, delayMs: number): unknown {
    const state: { fired: boolean; handle?: unknown } = { fired: false };
    const handle = this.options.setTimer(() => {
      state.fired = true;
      if (state.handle !== undefined) this.timers.delete(state.handle);
      if (!this.disposed) callback();
    }, delayMs);
    state.handle = handle;
    if (!state.fired) this.timers.add(handle);
    return handle;
  }

  private clearOwnedTimer(handle: unknown): void {
    if (!this.timers.delete(handle)) return;
    try {
      this.options.clearTimer(handle);
    } catch {
      // The generation latch prevents a hostile timer from restoring authority.
    }
  }

  private removePendingCommand(): void {
    const command = this.command;
    this.command = undefined;
    if (!command || !this.commandQueue) return;
    try {
      const index = this.commandQueueIndex;
      if (index >= 0 && this.commandQueue[index] === command) this.commandQueue.splice(index, 1);
      else {
        const current = this.commandQueue.indexOf(command);
        if (current >= 0) this.commandQueue.splice(current, 1);
      }
    } catch {
      // A poisoned publisher queue cannot keep this generation live.
    }
    this.commandQueue = undefined;
    this.commandQueueIndex = -1;
  }
}

/** Create the sole provisional adapter for one immutable projected GPT winner batch. */
export function createFirstDisplayGoogletagBatch(
  options: FirstDisplayGoogletagBatchOptions
): FirstDisplayGoogletagBatch {
  const owner = new FirstDisplayGoogletagBatchOwner(options);
  return Object.freeze({
    start: (callbacks: FirstDisplayGoogletagBatchCallbacks) => owner.start(callbacks),
    closeIngress: (committedSlotIds: readonly string[]) => owner.closeIngress(committedSlotIds),
    captureHandoff: () => owner.captureHandoff(),
    captureDiagnosticsHandoff: () => owner.captureDiagnosticsHandoff(),
    detachCommittedSlots: (slotIds: readonly string[]) => owner.detachCommittedSlots(slotIds),
    dispose: () => owner.dispose(),
  });
}
