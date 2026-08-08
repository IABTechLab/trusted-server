import type { GoogletagDiagnosticsFact } from '../../adapters/googletag';
import { log } from '../../core/log';
import type { GptDiagnosticsAdManagerIdentity, Size, TsjsApi } from '../../core/types';

import type { GptDiagnosticsSlotLike, GptRenderFacts } from './store';

export interface GptDiagnosticsObserverStore {
  markGptObserved(): void;
  recordSlotRequested(slot: GptDiagnosticsSlotLike): void;
  recordSlotResponseReceived(slot: GptDiagnosticsSlotLike): void;
  recordSlotRenderEnded(slot: GptDiagnosticsSlotLike, facts: GptRenderFacts): void;
  recordSlotOnload(slot: GptDiagnosticsSlotLike): void;
  recordImpressionViewable(slot: GptDiagnosticsSlotLike): void;
  recordSlotVisibilityChanged(slot: GptDiagnosticsSlotLike, percentage: number): void;
  recordPublisherRefresh(slots: GptDiagnosticsSlotLike[]): void;
}

interface ObserverLogger {
  warn(...args: unknown[]): unknown;
}

interface ObserverOptions {
  readonly logger?: ObserverLogger | undefined;
}

/** Consumes normalized facts from the sole GPT adapter without owning browser-global access. */
export class GptDiagnosticsObserver {
  private readonly store: GptDiagnosticsObserverStore;
  private readonly logger: ObserverLogger;
  private started = false;

  constructor(store: GptDiagnosticsObserverStore, options: ObserverOptions = {}) {
    this.store = store;
    this.logger = options.logger ?? log;
  }

  start(): void {
    if (this.started) return;
    this.started = true;
    this.handle('activation', () => this.store.markGptObserved());
  }

  consume(fact: Readonly<GoogletagDiagnosticsFact>): void {
    this.start();
    const slot = fact.slot as GptDiagnosticsSlotLike;
    switch (fact.kind) {
      case 'slotRequested':
        this.handle(fact.kind, () => this.store.recordSlotRequested(slot));
        return;
      case 'slotResponseReceived':
        this.handle(fact.kind, () => this.store.recordSlotResponseReceived(slot));
        return;
      case 'slotRenderEnded':
        this.handle(fact.kind, () =>
          this.store.recordSlotRenderEnded(slot, {
            isEmpty: fact.isEmpty,
            size: fact.size ? ([...fact.size] as Size) : undefined,
            isBackfill: fact.isBackfill,
            slotContentChanged: fact.slotContentChanged,
          })
        );
        return;
      case 'slotOnload':
        this.handle(fact.kind, () => this.store.recordSlotOnload(slot));
        return;
      case 'impressionViewable':
        this.handle(fact.kind, () => this.store.recordImpressionViewable(slot));
        return;
      case 'slotVisibilityChanged':
        this.handle(fact.kind, () =>
          this.store.recordSlotVisibilityChanged(
            slot,
            typeof fact.inViewPercentage === 'number' ? fact.inViewPercentage : Number.NaN
          )
        );
    }
  }

  private handle(
    kind: GoogletagDiagnosticsFact['kind'] | 'activation',
    callback: () => void
  ): void {
    try {
      callback();
    } catch (error) {
      try {
        this.logger.warn(`gpt diagnostics: ${kind} callback failed`, error);
      } catch {
        // Diagnostics logging cannot escape into adapter fact delivery.
      }
    }
  }

  private installRefreshObserver(pubads: GptPubAdsService): void {
    if (
      typeof pubads.refresh !== 'function' ||
      (pubads as GptPubAdsService & { [REFRESH_OBSERVER_INSTALLED]?: boolean })[
        REFRESH_OBSERVER_INSTALLED
      ]
    ) {
      return;
    }
    const originalRefresh = pubads.refresh;
    const { store, window: observerWindow } = this;
    pubads.refresh = function (this: unknown, ...args: unknown[]): unknown {
      try {
        if (
          !(
            observerWindow.tsjs?.adInitRefreshInProgress ||
            observerWindow.tsjs?.prebidRefreshDispatchInProgress
          )
        ) {
          // GPT treats a missing or null `slots` argument as "refresh every
          // slot", and `refresh(null, opts)` is the documented way to pass
          // options while doing so.
          const rawSlots = args.length === 0 || args[0] == null ? pubads.getSlots?.() : args[0];
          const slots = Array.isArray(rawSlots)
            ? rawSlots.filter(
                (slot): slot is GptDiagnosticsSlotLike => typeof slot === 'object' && slot !== null
              )
            : [];
          if (slots.length > 0) store.recordPublisherRefresh(slots);
        }
      } catch {
        // Diagnostics must not affect the original refresh.
      }
      return Reflect.apply(originalRefresh, this, args);
    };
    (pubads as GptPubAdsService & { [REFRESH_OBSERVER_INSTALLED]?: boolean })[
      REFRESH_OBSERVER_INSTALLED
    ] = true;
  }
}
