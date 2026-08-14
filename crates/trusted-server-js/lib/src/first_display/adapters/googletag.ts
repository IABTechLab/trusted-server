import type {
  FirstDisplayProjectionBidV1,
  FirstDisplayProjectionSlotV1,
  FirstDisplayProjectionV1,
} from '../leaf/projection';
import type { FirstDisplayGptProtocolV1 } from '../leaf/gpt_protocol';

export type FirstDisplayGptRenderResult = 'gam_empty' | 'nonempty_gam';
export type FirstDisplayGptFailureReason =
  | 'cycle_unattributable'
  | 'external_artifact_incompatible'
  | 'external_ready_timeout'
  | 'gpt_completion_timeout'
  | 'gpt_request_failed'
  | 'gpt_request_timeout'
  | 'slot_unresolved';

export interface FirstDisplayGptBoundCycleV1 {
  readonly bid: FirstDisplayProjectionBidV1;
  readonly element: HTMLElement;
  readonly ownership: 'publisher' | 'trusted_server';
  readonly physicalSlot: object;
  readonly placement: FirstDisplayProjectionSlotV1;
  readonly slotId: string;
}

export interface FirstDisplayGoogletagBatchCallbacks {
  readonly onBound: (cycle: FirstDisplayGptBoundCycleV1) => void;
  readonly onFailure: (slotId: string, reason: FirstDisplayGptFailureReason) => void;
  readonly onFirstAction: () => boolean;
  readonly onRenderEnded: (
    cycle: FirstDisplayGptBoundCycleV1,
    result: FirstDisplayGptRenderResult
  ) => void;
}

export interface FirstDisplayGoogletagBatch {
  readonly start: (callbacks: FirstDisplayGoogletagBatchCallbacks) => boolean;
  /** Stop provisional GPT ingress after every physical cycle is terminal. */
  readonly closeIngress: () => boolean;
  /** Capture exact terminal physical identities without changing disposal ownership. */
  readonly captureHandoff: () => readonly FirstDisplayGptBoundCycleV1[] | undefined;
  /** Exempt exactly the accepted slot identities from provisional destruction/restoration. */
  readonly detachCommittedSlots: (slotIds: readonly string[]) => boolean;
  readonly dispose: () => void;
}

export interface FirstDisplayGoogletagBatchOptions {
  readonly browser: Window & { googletag?: unknown };
  readonly clearTimer: (handle: unknown) => void;
  readonly document: Document;
  readonly projection: FirstDisplayProjectionV1;
  readonly protocol: Pick<
    FirstDisplayGptProtocolV1,
    'classifyRenderEnded' | 'deadlines' | 'requestPlan'
  >;
  readonly setTimer: (callback: () => void, delayMs: number) => unknown;
}

export type FirstDisplayGoogletagBatchInput = Omit<FirstDisplayGoogletagBatchOptions, 'protocol'>;

type ExternalObject = Record<PropertyKey, unknown>;

interface ActiveCycle extends FirstDisplayGptBoundCycleV1 {
  readonly elementId: string;
  readonly operations: readonly ('display' | 'refresh')[];
  readonly requestOperation: 0 | 1;
  completionTimer?: unknown;
  requestInvoked: boolean;
  requested: boolean;
  requestTimer?: unknown;
  settled: boolean;
}

interface TargetingRestorer {
  readonly installed: string;
  readonly key: string;
  readonly prior: readonly string[];
  readonly slot: object;
  valid: boolean;
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
  if (typeof callable !== 'function') throw new TypeError(`missing GPT method: ${String(key)}`);
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
  private readonly cycles = new Map<object, ActiveCycle>();
  private readonly createdSlots = new Set<object>();
  private readonly targetingObservers = new Map<object, () => void>();
  private readonly targetingRestorers: TargetingRestorer[] = [];
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
  private committedSlotsDetached = false;
  private readonly detachedSlots = new Set<object>();
  private firstAction = false;
  private targetingWriteDepth = 0;

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

  public closeIngress(): boolean {
    if (
      this.disposed ||
      this.ingressClosed ||
      !this.started ||
      [...this.cycles.values()].some((cycle) => !cycle.settled)
    ) {
      return false;
    }
    this.ingressClosed = true;
    this.removePendingCommand();
    for (const handle of [...this.timers]) this.clearOwnedTimer(handle);
    this.removeListener();
    for (const restoreObserver of this.targetingObservers.values()) {
      try {
        restoreObserver();
      } catch {
        // The closed generation cannot regain authority through a publisher replacement.
      }
    }
    this.targetingObservers.clear();
    return true;
  }

  public captureHandoff(): readonly FirstDisplayGptBoundCycleV1[] | undefined {
    if (this.disposed || !this.ingressClosed) return undefined;
    return Object.freeze(
      [...this.cycles.values()].map((cycle) =>
        Object.freeze({
          bid: cycle.bid,
          element: cycle.element,
          ownership: cycle.ownership,
          physicalSlot: cycle.physicalSlot,
          placement: cycle.placement,
          slotId: cycle.slotId,
        })
      )
    );
  }

  public detachCommittedSlots(slotIds: readonly string[]): boolean {
    if (this.disposed || !this.ingressClosed || this.committedSlotsDetached) return false;
    const requested = new Set(slotIds);
    if (requested.size !== slotIds.length) return false;
    const selected: object[] = [];
    for (const slotId of requested) {
      const matches = [...this.cycles.values()].filter((cycle) => cycle.slotId === slotId);
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
    for (const restoration of this.targetingRestorers.reverse()) {
      if (this.detachedSlots.has(restoration.slot)) continue;
      try {
        this.restorePublisherTargeting(restoration);
      } catch {
        // Publisher mutations and hostile GPT objects always win over restoration.
      }
    }
    this.targetingRestorers.length = 0;
    for (const restoreObserver of this.targetingObservers.values()) {
      try {
        restoreObserver();
      } catch {
        // Publisher replacement wins over observer restoration.
      }
    }
    this.targetingObservers.clear();
    const destroyableSlots = [...this.createdSlots].filter((slot) => !this.detachedSlots.has(slot));
    if (this.binding && destroyableSlots.length > 0) {
      try {
        call(this.binding, 'destroySlots', [destroyableSlots]);
      } catch {
        // Generation latching keeps failed physical cleanup inert.
      }
    }
    this.createdSlots.clear();
    this.cycles.clear();
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
    if (!service) throw new TypeError('invalid GPT pubads service');
    this.service = service;
    this.installListeners(service, callbacks);
    const existing = call(service, 'getSlots', []);
    if (!Array.isArray(existing)) throw new TypeError('invalid GPT slot inventory');
    const disabled = initialLoadDisabled(binding);

    for (const row of rows) {
      if (this.disposed) return;
      const element = resolveElement(this.options.document, row.placement);
      if (!element) {
        callbacks.onFailure(row.placement.slot, 'slot_unresolved');
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
        callbacks.onFailure(row.placement.slot, 'slot_unresolved');
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
        callbacks.onFailure(row.placement.slot, 'slot_unresolved');
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
        callbacks.onFailure(row.placement.slot, 'gpt_request_failed');
        continue;
      }
      const cycle: ActiveCycle = {
        bid: row.bid,
        element,
        elementId: element.id,
        operations: plan.operations,
        ownership,
        physicalSlot: slot,
        placement: row.placement,
        requestOperation: plan.requestOperation,
        requestInvoked: false,
        requested: false,
        settled: false,
        slotId: row.placement.slot,
      };
      this.cycles.set(slot, cycle);
      callbacks.onBound(
        Object.freeze({
          bid: cycle.bid,
          element: cycle.element,
          ownership: cycle.ownership,
          physicalSlot: cycle.physicalSlot,
          placement: cycle.placement,
          slotId: cycle.slotId,
        })
      );
      for (const [key, value] of targetingEntries(row.bid, row.placement)) {
        if (publisherSlot) this.journalPublisherTargeting(slot, key, value);
        this.writeTargeting(slot, 'setTargeting', [key, value]);
      }
    }

    for (const cycle of this.cycles.values()) {
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
              if (!callbacks.onFirstAction()) throw new TypeError('first action rejected');
            }
          }
          if (operation === 'display') call(binding, 'display', [cycle.elementId]);
          else call(service, 'refresh', [[cycle.physicalSlot], { changeCorrelator: false }]);
        }
      } catch {
        if (cycle.settled) continue;
        this.failCycle(cycle, callbacks, 'gpt_request_failed');
      }
    }
  }

  private installListeners(
    service: ExternalObject,
    callbacks: FirstDisplayGoogletagBatchCallbacks
  ): void {
    const requestedListener = (event: unknown): void => {
      if (this.disposed || this.requestedListener !== requestedListener) return;
      const slot = physicalSlot(member(event, 'slot'));
      const cycle = slot ? this.cycles.get(slot) : undefined;
      if (!cycle || cycle.settled || !cycle.requestInvoked) return;
      if (cycle.requested) {
        this.failCycle(cycle, callbacks, 'cycle_unattributable');
        return;
      }
      cycle.requested = true;
      if (cycle.requestTimer !== undefined) this.clearOwnedTimer(cycle.requestTimer);
    };
    const renderListener = (event: unknown): void => {
      if (this.disposed || this.renderListener !== renderListener) return;
      const slot = physicalSlot(member(event, 'slot'));
      const cycle = slot ? this.cycles.get(slot) : undefined;
      if (!cycle || cycle.settled || !cycle.requested) return;
      const result = this.options.protocol.classifyRenderEnded(
        Object.freeze({ isEmpty: member(event, 'isEmpty') })
      );
      if (!result) {
        this.failCycle(cycle, callbacks, 'gpt_request_failed');
        return;
      }
      cycle.settled = true;
      if (cycle.requestTimer !== undefined) this.clearOwnedTimer(cycle.requestTimer);
      if (cycle.completionTimer !== undefined) this.clearOwnedTimer(cycle.completionTimer);
      callbacks.onRenderEnded(
        Object.freeze({
          bid: cycle.bid,
          element: cycle.element,
          ownership: cycle.ownership,
          physicalSlot: cycle.physicalSlot,
          placement: cycle.placement,
          slotId: cycle.slotId,
        }),
        result
      );
    };
    this.requestedListener = requestedListener;
    this.renderListener = renderListener;
    call(service, 'addEventListener', ['slotRequested', requestedListener]);
    call(service, 'addEventListener', ['slotRenderEnded', renderListener]);
  }

  private removeListener(): void {
    if (!this.service) return;
    const requestedListener = this.requestedListener;
    const renderListener = this.renderListener;
    this.requestedListener = undefined;
    this.renderListener = undefined;
    try {
      if (requestedListener) {
        call(this.service, 'removeEventListener', ['slotRequested', requestedListener]);
      }
      if (renderListener) {
        call(this.service, 'removeEventListener', ['slotRenderEnded', renderListener]);
      }
    } catch {
      // The generation latch remains authoritative if GPT cannot detach physically.
    }
  }

  private failCycle(
    cycle: ActiveCycle,
    callbacks: FirstDisplayGoogletagBatchCallbacks,
    reason: FirstDisplayGptFailureReason
  ): void {
    if (cycle.settled) return;
    cycle.settled = true;
    if (cycle.requestTimer !== undefined) this.clearOwnedTimer(cycle.requestTimer);
    if (cycle.completionTimer !== undefined) this.clearOwnedTimer(cycle.completionTimer);
    callbacks.onFailure(cycle.slotId, reason);
  }

  private journalPublisherTargeting(slot: object, key: string, installed: string): void {
    const original = call(slot, 'getTargeting', [key]);
    if (!Array.isArray(original) || !original.every((value) => typeof value === 'string')) {
      throw new TypeError('invalid GPT targeting inventory');
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
    const restorers: Array<() => void> = [];
    try {
      for (const key of ['setTargeting', 'clearTargeting'] as const) {
        const original = member(slot, key);
        if (typeof original !== 'function') throw new TypeError(`missing GPT method: ${key}`);
        const prior = Object.getOwnPropertyDescriptor(slot, key);
        const invalidateTargeting = (targetingKey: string | undefined): void =>
          this.invalidateTargeting(slot, targetingKey);
        const isTrustedServerWrite = (): boolean => this.targetingWriteDepth > 0;
        const wrapper = function (this: unknown, ...arguments_: unknown[]): unknown {
          if (!isTrustedServerWrite() && this === slot) {
            const targetingKey = typeof arguments_[0] === 'string' ? arguments_[0] : undefined;
            invalidateTargeting(targetingKey);
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
          throw new TypeError(`cannot observe GPT method: ${key}`);
        }
        restorers.push(() => {
          const current = Object.getOwnPropertyDescriptor(slot, key);
          if (!current || !('value' in current) || current.value !== wrapper) return;
          if (prior) Reflect.defineProperty(slot, key, prior);
          else Reflect.deleteProperty(slot, key);
        });
      }
    } catch (error) {
      for (const restore of restorers.reverse()) restore();
      throw error;
    }
    this.targetingObservers.set(slot, () => {
      for (const restore of restorers.reverse()) restore();
    });
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
    const current = call(restoration.slot, 'getTargeting', [restoration.key]);
    if (!Array.isArray(current) || current.length !== 1 || current[0] !== restoration.installed) {
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
    this.targetingWriteDepth += 1;
    try {
      return call(slot, operation, arguments_);
    } finally {
      this.targetingWriteDepth -= 1;
    }
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
    closeIngress: () => owner.closeIngress(),
    captureHandoff: () => owner.captureHandoff(),
    detachCommittedSlots: (slotIds: readonly string[]) => owner.detachCommittedSlots(slotIds),
    dispose: () => owner.dispose(),
  });
}
