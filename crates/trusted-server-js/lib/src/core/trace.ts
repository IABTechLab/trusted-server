// Closure-private render diagnostics data for the hard-cutover runtime.
import type { RenderTraceDiagnostics, RenderTraceRecord } from './types';

const MAX_RENDER_LOG_ENTRIES = 200;

const MAX_RENDER_TRACE_SLOTS = 256;
const MAX_RENDER_TRACE_COUNTERS = 768;
const MAX_RENDER_TRACE_SUBSCRIBERS = 32;
const MAX_RENDER_TRACE_NOTIFICATIONS = 200;
const EMPTY_RENDER_TRACE_CURRENT = Object.freeze(
  Object.create(null) as Record<string, Readonly<RenderTraceRecord>>
);
const EMPTY_RENDER_TRACE_HISTORY = Object.freeze([]) as readonly Readonly<RenderTraceRecord>[];

type RenderTraceInputV1 = Omit<RenderTraceRecord, 'at' | 'count' | 'seq'>;
type RenderTraceUpdateV1 = Partial<Omit<RenderTraceRecord, 'at' | 'count' | 'seq' | 'slotId'>>;

/** Safe GPT fact shape admitted by the closure-private diagnostics bus. */
export interface RenderTraceGptFactV1 extends Readonly<Record<string, unknown>> {
  readonly kind:
    | 'slotRequested'
    | 'slotResponseReceived'
    | 'slotRenderEnded'
    | 'slotOnload'
    | 'impressionViewable'
    | 'slotVisibilityChanged';
  readonly slot: Readonly<{
    readonly token: string;
    readonly cycleOrdinal: number;
    readonly elementId?: string;
  }>;
  readonly isEmpty?: boolean;
  readonly inViewPercentage?: number;
}

/** Current registered-slot identity and presentation state for one safe GPT fact. */
export interface RenderTraceGptResolutionV1 {
  readonly slotId: string;
  readonly navigationGeneration: object;
  readonly traceToken: string;
  readonly elementId?: string;
  readonly visible?: boolean;
}

export interface RenderTraceRuntimeScheduler {
  readonly set: (callback: () => void, milliseconds: number) => unknown;
  readonly clear: (handle: unknown) => void;
}

export interface RenderTraceRuntimeOptions {
  readonly now?: () => number;
  readonly onOverflow?: (droppedNotifications: number) => void;
  readonly onPresentationError?: (error: unknown) => void;
  readonly onSubscriberError?: (error: unknown) => void;
  readonly schedule?: (callback: () => void) => () => void;
  readonly scheduler?: RenderTraceRuntimeScheduler;
}

export interface FirstDisplayTraceAdoptionV1 {
  readonly navigationGeneration: object;
  readonly nextSequence: number;
  readonly slots: readonly Readonly<{
    readonly bindings: readonly Readonly<{
      readonly cycleOrdinal: number;
      readonly historySequence: number;
      readonly state: 'completed' | 'retired';
      readonly token: string;
    }>[];
    readonly impressions: number;
    readonly records: readonly Readonly<RenderTraceRecord>[];
    readonly slotId: string;
  }>[];
}

/** Closure-private data channel made available only to the deferred presentation owner. */
export interface RenderTracePresentationSource {
  readonly current: RenderTraceDiagnostics['current'];
  readonly history: RenderTraceDiagnostics['history'];
  readonly subscribe: (listener: () => void) => () => void;
}

export interface RenderTracePresentationControls {
  readonly dispose: () => void;
}

export type RenderTracePresentationFactory = (
  source: RenderTracePresentationSource
) => RenderTracePresentationControls;

export interface RenderTraceRuntimeOwner {
  readonly api: RenderTraceDiagnostics;
  readonly diagnostics: RenderTraceDiagnostics;
  readonly record: (input: RenderTraceInputV1) => Readonly<RenderTraceRecord> | undefined;
  readonly enrich: (
    recordOrSequence: Readonly<RenderTraceRecord> | number,
    patch: RenderTraceUpdateV1
  ) => Readonly<RenderTraceRecord> | undefined;
  readonly prune: (slotId: string, sequence?: number) => boolean;
  readonly pruneNavigation: (navigationGeneration: object) => number;
  readonly observeGptFact: (
    fact: Readonly<RenderTraceGptFactV1>,
    resolve: (elementId: string | undefined) => RenderTraceGptResolutionV1 | undefined
  ) => void;
  readonly attachPresentation: (factory: RenderTracePresentationFactory) => () => void;
  readonly adoptFirstDisplay: (candidate: FirstDisplayTraceAdoptionV1) => boolean;
  readonly dispose: () => void;
}

export class DiagnosticsSubscriberLimitError extends Error {
  public readonly code = 'subscriber_capacity' as const;
  public readonly surface: 'renderTrace' | 'gpt';

  public constructor(surface: 'renderTrace' | 'gpt') {
    super('subscriber_capacity');
    this.name = 'DiagnosticsSubscriberLimitError';
    this.surface = surface;
  }
}

interface RenderTraceSubscription {
  readonly id: number;
  readonly listener: (record: Readonly<RenderTraceRecord>) => void;
}

interface PendingRenderTraceNotification {
  readonly record: Readonly<RenderTraceRecord>;
  readonly subscriberIds: readonly number[];
}

function copyRenderTraceRecord(record: Readonly<RenderTraceRecord>): Readonly<RenderTraceRecord> {
  const copy: Record<string, unknown> = {
    slotId: record.slotId,
    path: record.path,
    rendered: record.rendered,
  };
  const optional = [
    'elementId',
    'auctionId',
    'bidder',
    'adId',
    'bidId',
    'creativeId',
    'admHash',
    'servedFrom',
    'gamEmpty',
    'injected',
    'visible',
  ] as const;
  for (const key of optional) {
    const value = record[key];
    if (value !== undefined) copy[key] = value;
  }
  copy.count = record.count;
  copy.seq = record.seq;
  copy.at = record.at;
  return Object.freeze(copy) as unknown as Readonly<RenderTraceRecord>;
}

function scheduleRenderTraceTask(callback: () => void): () => void {
  const handle = globalThis.setTimeout(callback, 0);
  return (): void => globalThis.clearTimeout(handle);
}

function createRenderTraceOwner(options: RenderTraceRuntimeOptions): RenderTraceRuntimeOwner {
  const current = new Map<string, Readonly<RenderTraceRecord>>();
  const counts = new Map<string, number>();
  const history: Array<Readonly<RenderTraceRecord>> = [];
  const recordsBySequence = new Map<number, Readonly<RenderTraceRecord>>();
  const gptImpressions = new Map<
    string,
    {
      readonly baselineSequence: number | undefined;
      historySequence?: number;
      readonly navigationGeneration: object;
      reconciled?: boolean;
      readonly slotId: string;
      state: 'open' | 'completed' | 'retired';
      readonly token: string;
    }
  >();
  const subscribers = new Map<number, RenderTraceSubscription>();
  const pendingOrder: number[] = [];
  const pendingBySequence = new Map<number, PendingRenderTraceNotification>();
  let sequence = 0;
  let subscriberSequence = 0;
  let droppedNotifications = 0;
  let reportedDroppedNotifications = 0;
  let cancelScheduled: (() => void) | undefined;
  let presentationSubscriber: Readonly<{ generation: number; listener: () => void }> | undefined;
  let presentationGeneration = 0;
  let presentationPending:
    | Readonly<{
        subscriber: Readonly<{ generation: number; listener: () => void }>;
      }>
    | undefined;
  let cancelPresentationScheduled: (() => void) | undefined;
  let presentationControls: RenderTracePresentationControls | undefined;
  let invalidatePresentationSource: (() => void) | undefined;
  let presentationAttaching = false;
  let disposed = false;

  const adoptFirstDisplay = (candidate: FirstDisplayTraceAdoptionV1): boolean => {
    try {
      if (
        disposed ||
        sequence !== 0 ||
        current.size !== 0 ||
        history.length !== 0 ||
        recordsBySequence.size !== 0 ||
        counts.size !== 0 ||
        gptImpressions.size !== 0 ||
        typeof candidate !== 'object' ||
        candidate === null ||
        typeof candidate.navigationGeneration !== 'object' ||
        candidate.navigationGeneration === null ||
        !Number.isInteger(candidate.nextSequence) ||
        candidate.nextSequence < 1 ||
        candidate.nextSequence > 4_294_967_295 ||
        !Array.isArray(candidate.slots) ||
        candidate.slots.length > MAX_RENDER_TRACE_SLOTS
      ) {
        return false;
      }
      const adopted = new Map<
        string,
        Readonly<{
          bindings: readonly Readonly<{
            cycleOrdinal: number;
            historySequence: number;
            state: 'completed' | 'retired';
            token: string;
          }>[];
          impressions: number;
          records: readonly Readonly<RenderTraceRecord>[];
        }>
      >();
      const recordSequences = new Set<number>();
      const bindingKeys = new Set<string>();
      let bindingCount = 0;
      for (const slot of candidate.slots) {
        if (
          typeof slot !== 'object' ||
          slot === null ||
          typeof slot.slotId !== 'string' ||
          slot.slotId.length === 0 ||
          adopted.has(slot.slotId) ||
          !Number.isInteger(slot.impressions) ||
          slot.impressions < 0 ||
          slot.impressions > 4_294_967_295 ||
          !Array.isArray(slot.bindings) ||
          slot.bindings.length > 10 ||
          !Array.isArray(slot.records) ||
          slot.records.length > 10
        ) {
          return false;
        }
        const adoptedRecords: Readonly<RenderTraceRecord>[] = [];
        for (const record of slot.records) {
          if (
            typeof record !== 'object' ||
            record === null ||
            record.slotId !== slot.slotId ||
            !['auction', 'ssat', 'gam-refresh'].includes(record.path) ||
            typeof record.rendered !== 'boolean' ||
            !Number.isInteger(record.count) ||
            record.count < 1 ||
            record.count > slot.impressions ||
            !Number.isInteger(record.seq) ||
            record.seq < 1 ||
            record.seq >= candidate.nextSequence ||
            recordSequences.has(record.seq) ||
            typeof record.at !== 'number' ||
            !Number.isFinite(record.at) ||
            record.at < 0
          ) {
            return false;
          }
          recordSequences.add(record.seq);
          adoptedRecords.push(copyRenderTraceRecord(record));
        }
        const adoptedBindings: Array<
          Readonly<{
            cycleOrdinal: number;
            historySequence: number;
            state: 'completed' | 'retired';
            token: string;
          }>
        > = [];
        for (const binding of slot.bindings) {
          if (
            typeof binding !== 'object' ||
            binding === null ||
            typeof binding.token !== 'string' ||
            !/^gt1_[1-9a-z][0-9a-z]{0,6}$/.test(binding.token) ||
            !Number.isInteger(binding.cycleOrdinal) ||
            binding.cycleOrdinal < 1 ||
            binding.cycleOrdinal > 4_294_967_295 ||
            !Number.isInteger(binding.historySequence) ||
            !recordSequences.has(binding.historySequence) ||
            (binding.state !== 'completed' && binding.state !== 'retired')
          ) {
            return false;
          }
          const key = `${binding.token}:${binding.cycleOrdinal}`;
          if (bindingKeys.has(key)) return false;
          bindingKeys.add(key);
          bindingCount += 1;
          if (bindingCount > MAX_RENDER_TRACE_SLOTS) return false;
          adoptedBindings.push(
            Object.freeze({
              cycleOrdinal: binding.cycleOrdinal,
              historySequence: binding.historySequence,
              state: binding.state,
              token: binding.token,
            })
          );
        }
        if (
          adoptedRecords.length > 0 &&
          !adoptedRecords.some((record) => record.count === slot.impressions)
        ) {
          return false;
        }
        adopted.set(
          slot.slotId,
          Object.freeze({
            bindings: Object.freeze(adoptedBindings),
            impressions: slot.impressions,
            records: Object.freeze(adoptedRecords),
          })
        );
      }
      const adoptedHistory = [...adopted.entries()]
        .flatMap(([, value]) => value.records)
        .sort((left, right) => left.seq - right.seq);
      const adoptedCurrent = new Map<string, Readonly<RenderTraceRecord>>();
      for (const [slotId, value] of adopted) {
        const latest = value.records.reduce<Readonly<RenderTraceRecord> | undefined>(
          (candidateRecord, record) =>
            !candidateRecord || record.seq > candidateRecord.seq ? record : candidateRecord,
          undefined
        );
        if (latest) adoptedCurrent.set(slotId, latest);
      }
      const retainedSequences = new Set(
        [...adoptedHistory.slice(-MAX_RENDER_LOG_ENTRIES), ...adoptedCurrent.values()].map(
          (record) => record.seq
        )
      );
      if (
        [...adopted.values()].some((value) =>
          value.bindings.some((binding) => !retainedSequences.has(binding.historySequence))
        )
      ) {
        return false;
      }

      sequence = candidate.nextSequence - 1;
      for (const [slotId, value] of adopted) {
        if (value.impressions > 0) counts.set(slotId, value.impressions);
        const latest = adoptedCurrent.get(slotId);
        if (latest) current.set(slotId, latest);
        for (const binding of value.bindings) {
          gptImpressions.set(`${binding.token}:${binding.cycleOrdinal}`, {
            baselineSequence: undefined,
            historySequence: binding.historySequence,
            navigationGeneration: candidate.navigationGeneration,
            slotId,
            state: binding.state,
            token: binding.token,
          });
        }
      }
      for (const record of adoptedHistory.slice(-MAX_RENDER_LOG_ENTRIES)) {
        history.push(record);
      }
      for (const record of current.values()) recordsBySequence.set(record.seq, record);
      for (const record of history) recordsBySequence.set(record.seq, record);
      return true;
    } catch {
      return false;
    }
  };

  const schedule = (callback: () => void): (() => void) => {
    if (options.schedule) return options.schedule(callback);
    if (options.scheduler) {
      const handle = options.scheduler.set(callback, 0);
      return (): void => options.scheduler?.clear(handle);
    }
    return scheduleRenderTraceTask(callback);
  };

  const reportSubscriberError = (error: unknown): void => {
    try {
      options.onSubscriberError?.(error);
    } catch {
      // Diagnostics error reporting cannot affect correctness work.
    }
  };

  const reportPresentationError = (error: unknown): void => {
    try {
      options.onPresentationError?.(error);
    } catch {
      // Deferred presentation reporting cannot affect trace data ownership.
    }
  };

  const cancelPresentationTask = (): void => {
    const cancel = cancelPresentationScheduled;
    cancelPresentationScheduled = undefined;
    presentationPending = undefined;
    if (!cancel) return;
    try {
      cancel();
    } catch (error) {
      reportPresentationError(error);
    }
  };

  const notifyPresentation = (): void => {
    const subscriber = presentationSubscriber;
    if (disposed || !subscriber || presentationPending) return;
    const pending = Object.freeze({ subscriber });
    presentationPending = pending;
    try {
      const cancel = schedule(() => {
        if (presentationPending !== pending) return;
        cancelPresentationScheduled = undefined;
        presentationPending = undefined;
        if (disposed || presentationSubscriber !== subscriber) return;
        try {
          subscriber.listener();
        } catch (error) {
          reportPresentationError(error);
        }
      });
      if (typeof cancel !== 'function') throw new TypeError('invalid presentation scheduler');
      if (presentationPending === pending && presentationSubscriber === subscriber) {
        cancelPresentationScheduled = cancel;
      }
    } catch (error) {
      if (presentationPending === pending) presentationPending = undefined;
      cancelPresentationScheduled = undefined;
      reportPresentationError(error);
    }
  };

  const drain = (): void => {
    cancelScheduled = undefined;
    if (droppedNotifications !== reportedDroppedNotifications) {
      reportedDroppedNotifications = droppedNotifications;
      try {
        options.onOverflow?.(droppedNotifications);
      } catch {
        // Diagnostics-only overflow reporting stays inside the diagnostics task.
      }
    }
    while (!disposed && pendingOrder.length > 0) {
      const next = pendingOrder.shift();
      if (next === undefined) continue;
      const pending = pendingBySequence.get(next);
      pendingBySequence.delete(next);
      if (!pending) continue;
      for (const id of pending.subscriberIds) {
        const subscription = subscribers.get(id);
        if (!subscription) continue;
        try {
          subscription.listener(pending.record);
        } catch (error) {
          reportSubscriberError(error);
        }
      }
    }
  };

  const ensureDrain = (): boolean => {
    if (cancelScheduled) return true;
    try {
      const cancel = schedule(drain);
      if (typeof cancel !== 'function') throw new TypeError('invalid diagnostics scheduler');
      if (!disposed && pendingOrder.length > 0) cancelScheduled = cancel;
      return true;
    } catch {
      pendingOrder.length = 0;
      pendingBySequence.clear();
      cancelScheduled = undefined;
      return false;
    }
  };

  const enqueue = (record: Readonly<RenderTraceRecord>): void => {
    if (disposed || subscribers.size === 0) return;
    const pending = Object.freeze({
      record: copyRenderTraceRecord(record),
      subscriberIds: Object.freeze([...subscribers.keys()]),
    });
    if (pendingBySequence.has(record.seq)) {
      pendingBySequence.set(record.seq, pending);
      return;
    }
    if (pendingOrder.length >= MAX_RENDER_TRACE_NOTIFICATIONS) {
      const dropped = pendingOrder.shift();
      if (dropped !== undefined) pendingBySequence.delete(dropped);
      droppedNotifications += 1;
    }
    pendingOrder.push(record.seq);
    pendingBySequence.set(record.seq, pending);
    ensureDrain();
  };

  const retained = (record: Readonly<RenderTraceRecord>): boolean =>
    current.get(record.slotId)?.seq === record.seq ||
    history.some((candidate) => candidate.seq === record.seq);

  const trimCounters = (): void => {
    if (counts.size <= MAX_RENDER_TRACE_COUNTERS) return;
    const protectedSlotIds = new Set<string>();
    for (const slotId of current.keys()) protectedSlotIds.add(slotId);
    for (const traceRecord of history) protectedSlotIds.add(traceRecord.slotId);
    for (const impression of gptImpressions.values()) protectedSlotIds.add(impression.slotId);
    while (counts.size > MAX_RENDER_TRACE_COUNTERS) {
      let evicted = false;
      for (const slotId of counts.keys()) {
        if (protectedSlotIds.has(slotId)) continue;
        counts.delete(slotId);
        evicted = true;
        break;
      }
      if (!evicted) break;
    }
  };

  const record = (input: RenderTraceInputV1): Readonly<RenderTraceRecord> | undefined => {
    if (disposed) return undefined;
    if (input.path !== 'gam-refresh') {
      for (const impression of gptImpressions.values()) {
        if (
          impression.slotId !== input.slotId ||
          impression.state !== 'completed' ||
          impression.reconciled === true ||
          impression.historySequence === undefined ||
          current.get(input.slotId)?.seq !== impression.historySequence
        ) {
          continue;
        }
        const reconciled = enrich(impression.historySequence, input);
        if (reconciled) {
          impression.reconciled = true;
          return reconciled;
        }
      }
    }
    const previous = current.get(input.slotId);
    const evictedCurrentSlot =
      !previous && current.size >= MAX_RENDER_TRACE_SLOTS
        ? (current.keys().next().value as string | undefined)
        : undefined;
    let at: number;
    try {
      at = (options.now ?? Date.now)();
    } catch {
      at = Date.now();
    }
    const previousCount = counts.get(input.slotId) ?? 0;
    if (previousCount > 0) counts.delete(input.slotId);
    counts.set(input.slotId, previousCount + 1);
    const committed = copyRenderTraceRecord({
      ...input,
      count: previousCount + 1,
      seq: (sequence += 1),
      at,
    });
    if (evictedCurrentSlot !== undefined) {
      current.delete(evictedCurrentSlot);
    }
    current.set(committed.slotId, committed);
    recordsBySequence.set(committed.seq, committed);
    history.push(committed);
    if (history.length > MAX_RENDER_LOG_ENTRIES) {
      const evicted = history.shift();
      if (evicted && !retained(evicted)) recordsBySequence.delete(evicted.seq);
    }
    if (previous && !retained(previous)) recordsBySequence.delete(previous.seq);
    trimCounters();
    enqueue(committed);
    notifyPresentation();
    return committed;
  };

  const enrich = (
    recordOrSequence: Readonly<RenderTraceRecord> | number,
    patch: RenderTraceUpdateV1
  ): Readonly<RenderTraceRecord> | undefined => {
    if (disposed) return undefined;
    const targetSequence =
      typeof recordOrSequence === 'number' ? recordOrSequence : recordOrSequence?.seq;
    if (!Number.isSafeInteger(targetSequence) || targetSequence <= 0) return undefined;
    const existing = recordsBySequence.get(targetSequence);
    if (!existing) return undefined;
    const injected =
      existing.injected === true || patch.injected === true
        ? { injected: true as const }
        : existing.injected === false || patch.injected === false
          ? { injected: false as const }
          : {};
    const merged = {
      ...existing,
      ...patch,
      rendered:
        existing.rendered === true && patch.rendered === false
          ? true
          : (patch.rendered ?? existing.rendered),
      ...injected,
      slotId: existing.slotId,
      count: existing.count,
      seq: existing.seq,
      at: existing.at,
    } as RenderTraceRecord;
    const committed = copyRenderTraceRecord(merged);
    recordsBySequence.set(targetSequence, committed);
    if (current.get(existing.slotId)?.seq === targetSequence) {
      current.set(existing.slotId, committed);
    }
    const historyIndex = history.findIndex(({ seq }) => seq === targetSequence);
    if (historyIndex >= 0) history[historyIndex] = committed;
    enqueue(committed);
    notifyPresentation();
    return committed;
  };

  const prune = (slotId: string, expectedSequence?: number): boolean => {
    if (disposed || typeof slotId !== 'string') return false;
    let retired = false;
    for (const impression of gptImpressions.values()) {
      if (
        impression.slotId === slotId &&
        (expectedSequence === undefined ||
          impression.historySequence === expectedSequence ||
          impression.baselineSequence === expectedSequence)
      ) {
        impression.state = 'retired';
        retired = true;
      }
    }
    const existing = current.get(slotId);
    if (!existing || (expectedSequence !== undefined && existing.seq !== expectedSequence)) {
      return retired;
    }
    current.delete(slotId);
    if (!retained(existing)) recordsBySequence.delete(existing.seq);
    notifyPresentation();
    return true;
  };

  const pruneNavigation = (navigationGeneration: object): number => {
    if (disposed || typeof navigationGeneration !== 'object' || navigationGeneration === null) {
      return 0;
    }
    let retired = 0;
    let currentChanged = false;
    for (const impression of gptImpressions.values()) {
      if (
        impression.navigationGeneration !== navigationGeneration ||
        impression.state === 'retired'
      ) {
        continue;
      }
      impression.state = 'retired';
      retired += 1;
      const sequence = impression.historySequence ?? impression.baselineSequence;
      const existing = current.get(impression.slotId);
      if (sequence === undefined || existing?.seq !== sequence) continue;
      current.delete(impression.slotId);
      if (!retained(existing)) recordsBySequence.delete(existing.seq);
      currentChanged = true;
    }
    if (currentChanged) notifyPresentation();
    return retired;
  };

  const observeGptFact = (
    fact: Readonly<RenderTraceGptFactV1>,
    resolve: (elementId: string | undefined) => RenderTraceGptResolutionV1 | undefined
  ): void => {
    if (disposed || typeof resolve !== 'function') return;
    try {
      const token = fact.slot.token;
      const cycleOrdinal = fact.slot.cycleOrdinal;
      if (
        typeof token !== 'string' ||
        !/^gt1_[1-9a-z][0-9a-z]{0,6}$/.test(token) ||
        token.length > 11 ||
        Number.parseInt(token.slice(4), 36) > 4_294_967_295 ||
        !Number.isInteger(cycleOrdinal) ||
        cycleOrdinal < 1 ||
        cycleOrdinal > 4_294_967_295
      ) {
        return;
      }
      const key = `${token}:${cycleOrdinal}`;

      if (fact.kind === 'slotRequested') {
        if (gptImpressions.has(key)) return;
        const resolution = resolve(fact.slot.elementId);
        if (
          !resolution ||
          typeof resolution.slotId !== 'string' ||
          resolution.slotId === '' ||
          typeof resolution.navigationGeneration !== 'object' ||
          resolution.navigationGeneration === null ||
          resolution.traceToken !== token
        ) {
          return;
        }
        for (const impression of gptImpressions.values()) {
          if (impression.token === token && impression.state === 'open') return;
        }
        if (gptImpressions.size >= MAX_RENDER_TRACE_SLOTS) {
          let prunable: string | undefined;
          for (const [candidateKey, impression] of gptImpressions) {
            if (impression.state !== 'open') {
              prunable = candidateKey;
              break;
            }
          }
          if (prunable === undefined) return;
          gptImpressions.delete(prunable);
        }
        for (const impression of gptImpressions.values()) {
          if (
            impression.slotId === resolution.slotId &&
            (impression.state === 'completed' ||
              impression.navigationGeneration !== resolution.navigationGeneration ||
              impression.token !== token)
          ) {
            impression.state = 'retired';
          }
        }
        gptImpressions.set(key, {
          baselineSequence: current.get(resolution.slotId)?.seq,
          navigationGeneration: resolution.navigationGeneration,
          slotId: resolution.slotId,
          state: 'open',
          token,
        });
        return;
      }

      const impression = gptImpressions.get(key);
      if (!impression) return;
      if (fact.kind === 'slotResponseReceived') return;
      if (fact.kind === 'slotRenderEnded') {
        if (typeof fact.isEmpty !== 'boolean' || impression.state !== 'open') return;
        const resolution = resolve(fact.slot.elementId);
        if (
          !resolution ||
          resolution.slotId !== impression.slotId ||
          resolution.navigationGeneration !== impression.navigationGeneration ||
          resolution.traceToken !== token
        ) {
          impression.state = 'retired';
          return;
        }
        const latest = current.get(impression.slotId);
        const target =
          latest && latest.seq !== impression.baselineSequence
            ? latest
            : record({
                slotId: impression.slotId,
                path: 'gam-refresh',
                rendered: !fact.isEmpty,
                gamEmpty: fact.isEmpty,
                injected: false,
                ...(resolution?.elementId === undefined ? {} : { elementId: resolution.elementId }),
                ...(resolution?.visible === undefined
                  ? {}
                  : { visible: !fact.isEmpty && resolution.visible }),
                servedFrom: 'gam',
              });
        if (!target) return;
        const enriched = enrich(target, {
          rendered: !fact.isEmpty,
          gamEmpty: fact.isEmpty,
          injected: false,
          ...(target.servedFrom === undefined ? { servedFrom: 'gam' as const } : {}),
          ...(resolution?.elementId === undefined ? {} : { elementId: resolution.elementId }),
          ...(resolution?.visible === undefined
            ? {}
            : { visible: !fact.isEmpty && resolution.visible }),
        });
        impression.historySequence = enriched?.seq ?? target.seq;
        impression.state = 'completed';
        return;
      }

      const targetSequence = impression.historySequence;
      if (targetSequence === undefined) return;
      const update = (patch: RenderTraceUpdateV1): void => {
        if (impression.state !== 'retired') {
          enrich(targetSequence, patch);
          return;
        }
        const active = current.get(impression.slotId);
        const enriched = enrich(targetSequence, patch);
        if (active?.seq === targetSequence && enriched) current.set(impression.slotId, active);
      };
      if (fact.kind === 'impressionViewable') {
        update({ visible: true });
      } else if (
        fact.kind === 'slotVisibilityChanged' &&
        typeof fact.inViewPercentage === 'number' &&
        Number.isFinite(fact.inViewPercentage)
      ) {
        update({ visible: fact.inViewPercentage > 0 });
      } else if (fact.kind === 'slotOnload') {
        const resolution = resolve(fact.slot.elementId);
        if (
          resolution &&
          resolution.slotId === impression.slotId &&
          resolution.navigationGeneration === impression.navigationGeneration &&
          resolution.traceToken === impression.token &&
          resolution.visible !== undefined
        ) {
          update({ visible: resolution.visible });
        }
      }
    } catch {
      // GPT diagnostics cannot affect the committed render or adapter callback.
    }
  };

  const api: RenderTraceDiagnostics = Object.freeze({
    current: (): Readonly<Record<string, Readonly<RenderTraceRecord>>> => {
      const snapshot = Object.create(null) as Record<string, Readonly<RenderTraceRecord>>;
      for (const [slotId, traceRecord] of current) {
        Object.defineProperty(snapshot, slotId, {
          configurable: false,
          enumerable: true,
          value: copyRenderTraceRecord(traceRecord),
          writable: false,
        });
      }
      return Object.freeze(snapshot);
    },
    history: (): readonly Readonly<RenderTraceRecord>[] =>
      Object.freeze(history.map((traceRecord) => copyRenderTraceRecord(traceRecord))),
    subscribe: (listener: (record: Readonly<RenderTraceRecord>) => void): (() => void) => {
      if (typeof listener !== 'function')
        throw new TypeError('diagnostics listener must be callable');
      if (disposed) return () => undefined;
      if (subscribers.size >= MAX_RENDER_TRACE_SUBSCRIBERS) {
        throw new DiagnosticsSubscriberLimitError('renderTrace');
      }
      const id = (subscriberSequence += 1);
      const subscription = Object.freeze({ id, listener });
      subscribers.set(id, subscription);
      let active = true;
      return (): void => {
        if (!active) return;
        active = false;
        if (subscribers.get(id) === subscription) subscribers.delete(id);
      };
    },
  });

  const clearPresentationSubscriber = (): void => {
    presentationSubscriber = undefined;
    cancelPresentationTask();
  };

  const disposePresentationCandidate = (candidate: unknown): void => {
    try {
      if (typeof candidate !== 'object' || candidate === null) return;
      const descriptor = Object.getOwnPropertyDescriptor(candidate, 'dispose');
      if (descriptor && 'value' in descriptor && typeof descriptor.value === 'function') {
        Reflect.apply(descriptor.value, candidate, []);
      }
    } catch (error) {
      reportPresentationError(error);
    }
  };

  const validPresentationControls = (
    candidate: unknown
  ): candidate is RenderTracePresentationControls => {
    try {
      if (typeof candidate !== 'object' || candidate === null || !Object.isFrozen(candidate)) {
        return false;
      }
      const keys = Reflect.ownKeys(candidate);
      if (keys.length !== 1 || keys[0] !== 'dispose') return false;
      const descriptor = Object.getOwnPropertyDescriptor(candidate, 'dispose');
      return Boolean(
        descriptor?.enumerable && 'value' in descriptor && typeof descriptor.value === 'function'
      );
    } catch {
      return false;
    }
  };

  const attachPresentation = (factory: RenderTracePresentationFactory): (() => void) => {
    if (typeof factory !== 'function') {
      throw new TypeError('render trace presentation factory must be callable');
    }
    if (disposed || presentationControls || presentationAttaching) {
      throw new TypeError('render trace presentation is unavailable');
    }
    presentationAttaching = true;
    let sourceLive = true;
    const invalidateSource = (): void => {
      sourceLive = false;
    };
    invalidatePresentationSource = invalidateSource;
    const source = Object.freeze({
      current: (): Readonly<Record<string, Readonly<RenderTraceRecord>>> =>
        sourceLive && !disposed ? api.current() : EMPTY_RENDER_TRACE_CURRENT,
      history: (): readonly Readonly<RenderTraceRecord>[] =>
        sourceLive && !disposed ? api.history() : EMPTY_RENDER_TRACE_HISTORY,
      subscribe: (listener: () => void): (() => void) => {
        if (typeof listener !== 'function') {
          throw new TypeError('render trace presentation listener must be callable');
        }
        if (
          disposed ||
          !sourceLive ||
          presentationSubscriber ||
          (!presentationAttaching && !presentationControls)
        ) {
          throw new TypeError('render trace presentation subscription is unavailable');
        }
        const subscription = Object.freeze({
          generation: (presentationGeneration += 1),
          listener,
        });
        presentationSubscriber = subscription;
        let active = true;
        return (): void => {
          if (!active) return;
          active = false;
          if (presentationSubscriber === subscription) clearPresentationSubscriber();
        };
      },
    }) satisfies RenderTracePresentationSource;
    let candidate: unknown;
    try {
      candidate = factory(source);
      if (!validPresentationControls(candidate)) {
        throw new TypeError('render trace presentation controls are malformed');
      }
      if (!presentationSubscriber) {
        throw new TypeError('render trace presentation subscription is unavailable');
      }
      const controls = candidate;
      presentationControls = controls;
      let active = true;
      return (): void => {
        if (!active) return;
        active = false;
        if (presentationControls !== controls) return;
        presentationControls = undefined;
        if (invalidatePresentationSource === invalidateSource) {
          invalidatePresentationSource = undefined;
        }
        invalidateSource();
        clearPresentationSubscriber();
        disposePresentationCandidate(controls);
      };
    } catch (error) {
      if (invalidatePresentationSource === invalidateSource) {
        invalidatePresentationSource = undefined;
      }
      invalidateSource();
      clearPresentationSubscriber();
      disposePresentationCandidate(candidate);
      throw error;
    } finally {
      presentationAttaching = false;
    }
  };

  const dispose = (): void => {
    if (disposed) return;
    disposed = true;
    const controls = presentationControls;
    presentationControls = undefined;
    invalidatePresentationSource?.();
    invalidatePresentationSource = undefined;
    clearPresentationSubscriber();
    if (controls) disposePresentationCandidate(controls);
    try {
      cancelScheduled?.();
    } catch {
      // The disposed latch suppresses a hostile late callback.
    }
    cancelScheduled = undefined;
    subscribers.clear();
    pendingOrder.length = 0;
    pendingBySequence.clear();
    current.clear();
    history.length = 0;
    recordsBySequence.clear();
    counts.clear();
    gptImpressions.clear();
  };

  return Object.freeze({
    api,
    diagnostics: api,
    record,
    enrich,
    prune,
    pruneNavigation,
    observeGptFact,
    attachPresentation,
    adoptFirstDisplay,
    dispose,
  });
}

/** Data-only takeover trace owner; contains no DOM presentation behavior. */
export function createRenderTraceStore(
  options: RenderTraceRuntimeOptions = {}
): RenderTraceRuntimeOwner {
  return createRenderTraceOwner(options);
}
