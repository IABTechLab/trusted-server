import type { GoogletagDiagnosticsFact } from '../../adapters/googletag';
import { log } from '../../core/log';
import type { Size } from '../../core/types';

import type { GptDiagnosticsSlotLike, GptRenderFacts } from './store';

function renderFacts(fact: Readonly<GoogletagDiagnosticsFact>): GptRenderFacts {
  const adManager = fact.adManager;
  return {
    isEmpty: fact.isEmpty,
    size: fact.size ? ([...fact.size] as Size) : undefined,
    isBackfill: fact.isBackfill,
    slotContentChanged: fact.slotContentChanged,
    adManager: adManager
      ? {
          ...adManager,
          yieldGroupIds: adManager.yieldGroupIds ? [...adManager.yieldGroupIds] : undefined,
          companyIds: adManager.companyIds ? [...adManager.companyIds] : undefined,
        }
      : undefined,
  };
}

export interface GptDiagnosticsObserverStore {
  markGptObserved(): void;
  recordSlotRequested(slot: GptDiagnosticsSlotLike, timestampMs?: number): void;
  recordSlotResponseReceived(slot: GptDiagnosticsSlotLike, timestampMs?: number): void;
  recordSlotRenderEnded(
    slot: GptDiagnosticsSlotLike,
    facts: GptRenderFacts,
    timestampMs?: number
  ): void;
  recordSlotOnload(slot: GptDiagnosticsSlotLike, timestampMs?: number): void;
  recordImpressionViewable(slot: GptDiagnosticsSlotLike, timestampMs?: number): void;
  recordSlotVisibilityChanged(
    slot: GptDiagnosticsSlotLike,
    percentage: number,
    timestampMs?: number
  ): void;
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
  private observed = false;

  constructor(store: GptDiagnosticsObserverStore, options: ObserverOptions = {}) {
    this.store = store;
    this.logger = options.logger ?? log;
  }

  start(): void {
    if (this.started) return;
    this.started = true;
  }

  consume(fact: Readonly<GoogletagDiagnosticsFact>): void {
    this.start();
    if (!this.observed) {
      this.observed = true;
      this.handle('observation', () => this.store.markGptObserved());
    }
    const slot = fact.slot as GptDiagnosticsSlotLike;
    const observedAtMs =
      typeof fact.observedAtMs === 'number' && Number.isFinite(fact.observedAtMs)
        ? fact.observedAtMs
        : undefined;
    switch (fact.kind) {
      case 'slotRequested':
        this.handle(fact.kind, () =>
          observedAtMs === undefined
            ? this.store.recordSlotRequested(slot)
            : this.store.recordSlotRequested(slot, observedAtMs)
        );
        return;
      case 'slotResponseReceived':
        this.handle(fact.kind, () =>
          observedAtMs === undefined
            ? this.store.recordSlotResponseReceived(slot)
            : this.store.recordSlotResponseReceived(slot, observedAtMs)
        );
        return;
      case 'slotRenderEnded':
        this.handle(fact.kind, () =>
          observedAtMs === undefined
            ? this.store.recordSlotRenderEnded(slot, renderFacts(fact))
            : this.store.recordSlotRenderEnded(slot, renderFacts(fact), observedAtMs)
        );
        return;
      case 'slotOnload':
        this.handle(fact.kind, () =>
          observedAtMs === undefined
            ? this.store.recordSlotOnload(slot)
            : this.store.recordSlotOnload(slot, observedAtMs)
        );
        return;
      case 'impressionViewable':
        this.handle(fact.kind, () =>
          observedAtMs === undefined
            ? this.store.recordImpressionViewable(slot)
            : this.store.recordImpressionViewable(slot, observedAtMs)
        );
        return;
      case 'slotVisibilityChanged':
        this.handle(fact.kind, () =>
          observedAtMs === undefined
            ? this.store.recordSlotVisibilityChanged(
                slot,
                typeof fact.inViewPercentage === 'number' ? fact.inViewPercentage : Number.NaN
              )
            : this.store.recordSlotVisibilityChanged(
                slot,
                typeof fact.inViewPercentage === 'number' ? fact.inViewPercentage : Number.NaN,
                observedAtMs
              )
        );
    }
  }

  private handle(
    kind: GoogletagDiagnosticsFact['kind'] | 'observation',
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
}
