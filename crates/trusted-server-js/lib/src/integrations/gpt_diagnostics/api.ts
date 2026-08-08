import type { GptDiagnosticsApi, GptDiagnosticsExportV1 } from '../../core/types';
import { DiagnosticsSubscriberLimitError } from '../../core/trace';

import type { GptDiagnosticsBindingManager } from './binding';
import type { GptDiagnosticsStoreSnapshot } from './store';

interface ApiStore {
  snapshot(): GptDiagnosticsStoreSnapshot;
  subscribe(listener: () => void): () => void;
  recordTrustedServerOpportunity(
    slot: GptDiagnosticsSlotHandle,
    auctionSlotId: string,
    opportunity: GptDiagnosticsTrustedServerOpportunity,
    trustedServerAuctionId?: string,
    requestedSlotSizes?: ReadonlyArray<readonly [number, number]>
  ): void;
  recordPrebidRefresh(slots: GptDiagnosticsSlotHandle[]): void;
  recordTrustedServerCreativeRequest(auctionSlotId: string): number | undefined;
  recordTrustedServerCreativeResponse(attemptId: number): void;
  recordTrustedServerCreativeFailure(
    attemptId: number,
    reason: GptDiagnosticsCreativeFailure
  ): void;
}

interface ApiBindingManager {
  exportBinding: GptDiagnosticsBindingManager['exportBinding'];
  subscribe(listener: () => void): () => void;
}

interface PresentationControls {
  show(): void;
  hide(): void;
}

type ApiWindow = Window & {
  Blob: typeof Blob;
  URL: typeof URL;
};

interface ApiOptions {
  window?: ApiWindow | undefined;
  document?: Document | undefined;
  now?: (() => Date) | undefined;
  schedule?: ((callback: () => void) => () => void) | undefined;
}

type ApiListener = (snapshot: GptDiagnosticsExportV1) => void;
const MAX_API_SUBSCRIBERS = 32;

interface PendingNotification {
  readonly snapshot: GptDiagnosticsExportV1;
  readonly subscriberIds: readonly number[];
}

function scheduleTask(callback: () => void): () => void {
  const handle = globalThis.setTimeout(callback, 0);
  return () => globalThis.clearTimeout(handle);
}

function cloneExportSnapshot(snapshot: GptDiagnosticsExportV1): GptDiagnosticsExportV1 {
  return {
    version: snapshot.version,
    capturedAt: snapshot.capturedAt,
    page: { ...snapshot.page },
    slots: snapshot.slots.map((slot) => ({
      ...slot,
      binding: { ...slot.binding },
      requests: slot.requests.map((cycle) => ({
        ...cycle,
        durations: { ...cycle.durations },
        requestedSlotSizes: cycle.requestedSlotSizes?.map((size) => [...size]),
        size: cycle.size ? [...cycle.size] : undefined,
        observedSlotSize: cycle.observedSlotSize ? [...cycle.observedSlotSize] : undefined,
        adManager: cycle.adManager
          ? {
              ...cycle.adManager,
              yieldGroupIds: cycle.adManager.yieldGroupIds
                ? [...cycle.adManager.yieldGroupIds]
                : undefined,
              companyIds: cycle.adManager.companyIds ? [...cycle.adManager.companyIds] : undefined,
            }
          : undefined,
        trustedServerCreativeFailures: cycle.trustedServerCreativeFailures
          ? [...cycle.trustedServerCreativeFailures]
          : undefined,
      })),
    })),
    callbackIssues: snapshot.callbackIssues.map((issue) => ({ ...issue })),
    ...(snapshot.attributionIssues === undefined
      ? {}
      : { attributionIssues: snapshot.attributionIssues.map((issue) => ({ ...issue })) }),
    coverage: Object.fromEntries(
      Object.entries(snapshot.coverage).map(([kind, counters]) => [kind, { ...counters }])
    ) as GptDiagnosticsExportV1['coverage'],
    metadata: { ...snapshot.metadata },
  };
}

function safelyRecord(action: () => void): void {
  try {
    action();
  } catch {
    // Diagnostics must not alter delivery.
  }
}

function safelyCreateAttempt(action: () => number | undefined): number | undefined {
  try {
    return action();
  } catch {
    return undefined;
  }
}

/** Owns the public read-only diagnostics API and its source subscriptions. */
export class GptDiagnosticsApiController {
  readonly api: GptDiagnosticsApi;
  /** Internal evidence channel for Trusted Server integration modules. */
  readonly recorder: GptDiagnosticsRecorder;

  private readonly store: ApiStore;
  private readonly bindings: ApiBindingManager;
  private readonly presentation: PresentationControls;
  private readonly window: ApiWindow;
  private readonly document: Document;
  private readonly now: () => Date;
  private readonly schedule: (callback: () => void) => () => void;
  private readonly listeners = new Map<number, ApiListener>();
  private readonly unsubscribeStore: () => void;
  private readonly unsubscribeBindings: () => void;
  private pending: PendingNotification | undefined;
  private cancelScheduled: (() => void) | undefined;
  private nextSubscriberId = 0;
  private destroyed = false;

  constructor(
    store: ApiStore,
    bindings: ApiBindingManager,
    presentation: PresentationControls,
    options: ApiOptions = {}
  ) {
    this.store = store;
    this.bindings = bindings;
    this.presentation = presentation;
    this.window = options.window ?? (window as unknown as ApiWindow);
    this.document = options.document ?? document;
    this.now = options.now ?? (() => new Date());
    this.schedule = options.schedule ?? scheduleTask;
    this.unsubscribeStore = this.store.subscribe(() => this.scheduleNotification());
    this.unsubscribeBindings = this.bindings.subscribe(() => this.scheduleNotification());

    this.api = Object.freeze({
      snapshot: () => this.snapshot(),
      export: () => this.download(),
      subscribe: (listener: ApiListener) => this.subscribe(listener),
      show: () => this.presentation.show(),
      hide: () => this.presentation.hide(),
    });
  }

  snapshot(): GptDiagnosticsExportV1 {
    const store = this.store.snapshot();
    const slots = Object.freeze(
      store.slots.map((slot) =>
        Object.freeze({
          runtimeSlotNumber: slot.runtimeSlotNumber,
          slotElementId: slot.slotElementId,
          adUnitPath: slot.adUnitPath,
          binding: Object.freeze({ ...this.bindings.exportBinding(slot.runtimeSlotNumber) }),
          currentVisibilityPercentage: slot.currentVisibilityPercentage,
          maximumVisibilityPercentage: slot.maximumVisibilityPercentage,
          requests: Object.freeze(
            slot.requests.map((cycle) =>
              Object.freeze({
                ...cycle,
                durations: Object.freeze({ ...cycle.durations }),
                size: cycle.size ? Object.freeze([...cycle.size]) : undefined,
              })
            )
          ),
        })
      )
    );
    const callbackIssues = Object.freeze(
      store.callbackIssues.map((issue) => Object.freeze({ ...issue }))
    );
    const coverage = Object.freeze(
      Object.fromEntries(
        Object.entries(store.coverage).map(([kind, counters]) => [
          kind,
          Object.freeze({ ...counters }),
        ])
      )
    ) as GptDiagnosticsExportV1['coverage'];
    return Object.freeze({
      version: 1,
      capturedAt: this.now().toISOString(),
      page: Object.freeze({
        origin: this.window.location.origin,
        pathname: this.window.location.pathname,
      }),
      slots,
      callbackIssues,
      coverage,
      metadata: Object.freeze({ ...store.metadata }),
    }) as GptDiagnosticsExportV1;
  }

  destroy(): void {
    if (this.destroyed) return;
    this.destroyed = true;
    this.unsubscribeStore();
    this.unsubscribeBindings();
    try {
      this.cancelScheduled?.();
    } catch {
      // The destroyed latch suppresses a hostile late scheduler callback.
    }
    this.cancelScheduled = undefined;
    this.pending = undefined;
    this.listeners.clear();
  }

  private subscribe(listener: ApiListener): () => void {
    if (typeof listener !== 'function') throw new TypeError('Diagnostics listener must be callable');
    if (this.destroyed) return () => undefined;
    if (this.listeners.size >= MAX_API_SUBSCRIBERS) {
      throw new DiagnosticsSubscriberLimitError('gpt');
    }
    const id = (this.nextSubscriberId += 1);
    this.listeners.set(id, listener);
    let active = true;
    return () => {
      if (!active) return;
      active = false;
      this.listeners.delete(id);
    };
  }

  private download(): void {
    const snapshot = this.snapshot();
    const blob = new this.window.Blob([JSON.stringify(snapshot, null, 2)], {
      type: 'application/json',
    });
    const objectUrl = this.window.URL.createObjectURL(blob);
    const anchor = this.document.createElement('a');
    anchor.href = objectUrl;
    anchor.download = `trusted-server-gpt-diagnostics-${snapshot.capturedAt.replace(/[:.]/g, '-')}.json`;
    anchor.hidden = true;
    this.document.body?.append(anchor);

    try {
      anchor.click();
    } finally {
      anchor.remove();
      this.window.URL.revokeObjectURL(objectUrl);
    }
  }

  private scheduleNotification(): void {
    if (this.destroyed || this.listeners.size === 0) return;
    const pending = Object.freeze({
      snapshot: this.snapshot(),
      subscriberIds: Object.freeze([...this.listeners.keys()]),
    });
    this.pending = pending;
    if (this.cancelScheduled) return;
    try {
      const cancel = this.schedule(() => {
        this.cancelScheduled = undefined;
        const notification = this.pending;
        this.pending = undefined;
        if (this.destroyed || !notification) return;
        for (const id of notification.subscriberIds) {
          const listener = this.listeners.get(id);
          if (!listener) continue;
          try {
            listener(notification.snapshot);
          } catch {
            // One API subscriber must not block the rest.
          }
        }
      });
      if (typeof cancel !== 'function') throw new TypeError('Invalid diagnostics scheduler');
      if (!this.destroyed && this.pending) this.cancelScheduled = cancel;
    } catch {
      this.cancelScheduled = undefined;
      this.pending = undefined;
    }
  }
}
