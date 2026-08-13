import type {
  GptDiagnosticsApi,
  GptDiagnosticsCreativeFailure,
  GptDiagnosticsExportV1,
  GptDiagnosticsRecorder,
  GptDiagnosticsSlotHandle,
  GptDiagnosticsTrustedServerOpportunity,
} from '../../core/types';

import type { GptDiagnosticsBindingManager } from './binding';
import type { GptDiagnosticsStoreSnapshot } from './store';

interface ApiStore {
  snapshot(): GptDiagnosticsStoreSnapshot;
  subscribe(listener: () => void): () => void;
  recordTrustedServerOpportunity(
    slot: GptDiagnosticsSlotHandle,
    auctionSlotId: string,
    opportunity: GptDiagnosticsTrustedServerOpportunity,
    trustedServerAuctionId?: string
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
  window?: ApiWindow;
  document?: Document;
  now?: () => Date;
  schedule?: (callback: () => void) => void;
}

type ApiListener = (snapshot: GptDiagnosticsExportV1) => void;

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
  private readonly schedule: (callback: () => void) => void;
  private readonly listeners = new Set<ApiListener>();
  private readonly unsubscribeStore: () => void;
  private readonly unsubscribeBindings: () => void;
  private notificationScheduled = false;
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
    this.schedule = options.schedule ?? ((callback) => queueMicrotask(callback));
    this.unsubscribeStore = this.store.subscribe(() => this.scheduleNotification());
    this.unsubscribeBindings = this.bindings.subscribe(() => this.scheduleNotification());

    this.api = {
      snapshot: () => this.snapshot(),
      export: () => this.download(),
      subscribe: (listener) => this.subscribe(listener),
      show: () => this.presentation.show(),
      hide: () => this.presentation.hide(),
    };

    this.recorder = {
      recordTrustedServerOpportunity: (slot, auctionSlotId, opportunity, trustedServerAuctionId) =>
        safelyRecord(() => {
          if (trustedServerAuctionId === undefined) {
            this.store.recordTrustedServerOpportunity(slot, auctionSlotId, opportunity);
          } else {
            this.store.recordTrustedServerOpportunity(
              slot,
              auctionSlotId,
              opportunity,
              trustedServerAuctionId
            );
          }
        }),
      recordPrebidRefresh: (slots) => safelyRecord(() => this.store.recordPrebidRefresh(slots)),
      recordTrustedServerCreativeRequest: (auctionSlotId) =>
        safelyCreateAttempt(() => this.store.recordTrustedServerCreativeRequest(auctionSlotId)),
      recordTrustedServerCreativeResponse: (attemptId) =>
        safelyRecord(() => this.store.recordTrustedServerCreativeResponse(attemptId)),
      recordTrustedServerCreativeFailure: (attemptId, reason) =>
        safelyRecord(() => this.store.recordTrustedServerCreativeFailure(attemptId, reason)),
    };
  }

  snapshot(): GptDiagnosticsExportV1 {
    const store = this.store.snapshot();
    return {
      version: 1,
      capturedAt: this.now().toISOString(),
      page: {
        origin: this.window.location.origin,
        pathname: this.window.location.pathname,
      },
      slots: store.slots.map((slot) => ({
        runtimeSlotNumber: slot.runtimeSlotNumber,
        slotElementId: slot.slotElementId,
        adUnitPath: slot.adUnitPath,
        binding: this.bindings.exportBinding(slot.runtimeSlotNumber),
        currentVisibilityPercentage: slot.currentVisibilityPercentage,
        maximumVisibilityPercentage: slot.maximumVisibilityPercentage,
        requests: slot.requests.map((cycle) => ({
          ...cycle,
          durations: { ...cycle.durations },
          size: cycle.size ? [...cycle.size] : undefined,
          observedSlotSize: cycle.observedSlotSize ? [...cycle.observedSlotSize] : undefined,
          adManager: cycle.adManager
            ? {
                ...cycle.adManager,
                yieldGroupIds: cycle.adManager.yieldGroupIds
                  ? [...cycle.adManager.yieldGroupIds]
                  : undefined,
                companyIds: cycle.adManager.companyIds
                  ? [...cycle.adManager.companyIds]
                  : undefined,
              }
            : undefined,
          trustedServerCreativeFailures: cycle.trustedServerCreativeFailures
            ? [...cycle.trustedServerCreativeFailures]
            : undefined,
        })),
      })),
      callbackIssues: store.callbackIssues.map((issue) => ({ ...issue })),
      attributionIssues: store.attributionIssues.map((issue) => ({ ...issue })),
      coverage: Object.fromEntries(
        Object.entries(store.coverage).map(([kind, counters]) => [kind, { ...counters }])
      ) as GptDiagnosticsExportV1['coverage'],
      metadata: { ...store.metadata },
    };
  }

  destroy(): void {
    if (this.destroyed) return;
    this.destroyed = true;
    this.unsubscribeStore();
    this.unsubscribeBindings();
    this.listeners.clear();
  }

  private subscribe(listener: ApiListener): () => void {
    if (this.destroyed) return () => undefined;
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
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
    if (this.destroyed || this.notificationScheduled) return;
    this.notificationScheduled = true;
    this.schedule(() => {
      this.notificationScheduled = false;
      if (this.destroyed) return;

      // One snapshot per notification: every subscriber must see the same
      // `capturedAt` and the same derived delivery states, and deriving it once
      // keeps the cost independent of the subscriber count.
      let snapshot: GptDiagnosticsExportV1;
      try {
        snapshot = this.snapshot();
      } catch {
        // A snapshot failure must not escape the scheduled callback.
        return;
      }

      for (const listener of this.listeners) {
        try {
          listener(cloneExportSnapshot(snapshot));
        } catch {
          // One API subscriber must not block the rest.
        }
      }
    });
  }
}
