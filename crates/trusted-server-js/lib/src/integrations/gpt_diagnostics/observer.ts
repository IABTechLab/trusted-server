import { log } from '../../core/log';
import type { Size } from '../../core/types';

import type { GptDiagnosticsSlotLike, GptRenderFacts } from './store';

export interface GptDiagnosticsObserverStore {
  markGptObserved(): void;
  recordSlotRequested(slot: GptDiagnosticsSlotLike): void;
  recordSlotResponseReceived(slot: GptDiagnosticsSlotLike): void;
  recordSlotRenderEnded(slot: GptDiagnosticsSlotLike, facts: GptRenderFacts): void;
  recordSlotOnload(slot: GptDiagnosticsSlotLike): void;
  recordImpressionViewable(slot: GptDiagnosticsSlotLike): void;
  recordSlotVisibilityChanged(slot: GptDiagnosticsSlotLike, percentage: number): void;
}

interface GptEvent {
  slot: GptDiagnosticsSlotLike;
}

interface GptRenderEvent extends GptEvent {
  isEmpty?: boolean;
  size?: unknown;
  isBackfill?: boolean;
  slotContentChanged?: boolean;
}

interface GptVisibilityEvent extends GptEvent {
  inViewPercentage: number;
}

type GptEventName =
  | 'slotRequested'
  | 'slotResponseReceived'
  | 'slotRenderEnded'
  | 'slotOnload'
  | 'impressionViewable'
  | 'slotVisibilityChanged';

type GptEventListener = (event: GptEvent) => void;

interface GptPubAdsService {
  addEventListener(name: GptEventName, listener: GptEventListener): void;
}

interface GptCommandQueue {
  push(...callbacks: Array<() => void>): number;
}

interface GoogletagLike {
  cmd: GptCommandQueue;
  pubads?: () => GptPubAdsService;
}

export interface GptObserverWindow {
  googletag?: GoogletagLike;
}

interface ObserverLogger {
  warn(...args: unknown[]): void;
}

interface ObserverOptions {
  window?: GptObserverWindow;
  logger?: ObserverLogger;
}

function normalizeSize(value: unknown): Size | undefined {
  if (
    !Array.isArray(value) ||
    value.length !== 2 ||
    typeof value[0] !== 'number' ||
    typeof value[1] !== 'number' ||
    !Number.isFinite(value[0]) ||
    !Number.isFinite(value[1])
  ) {
    return undefined;
  }

  return [value[0], value[1]];
}

/** Installs documented GPT event listeners through `googletag.cmd`. */
export class GptDiagnosticsObserver {
  private readonly store: GptDiagnosticsObserverStore;
  private readonly window: GptObserverWindow;
  private readonly logger: ObserverLogger;
  private queued = false;
  private installed = false;

  constructor(store: GptDiagnosticsObserverStore, options: ObserverOptions = {}) {
    this.store = store;
    this.window = options.window ?? (window as unknown as GptObserverWindow);
    this.logger = options.logger ?? log;
  }

  install(): void {
    if (this.queued || this.installed) return;
    this.queued = true;

    try {
      const googletag = (this.window.googletag ??= { cmd: [] });
      googletag.cmd ??= [];
      googletag.cmd.push(() => this.installWhenReady(googletag));
    } catch (error) {
      this.queued = false;
      this.logger.warn('gpt diagnostics: command queue installation failed', error);
    }
  }

  private installWhenReady(googletag: GoogletagLike): void {
    if (this.installed) return;

    try {
      const pubads = googletag.pubads?.();
      if (!pubads || typeof pubads.addEventListener !== 'function') {
        this.logger.warn('gpt diagnostics: PubAdsService unavailable');
        return;
      }

      pubads.addEventListener('slotRequested', (event) => {
        this.handle('slotRequested', () => this.store.recordSlotRequested(event.slot));
      });
      pubads.addEventListener('slotResponseReceived', (event) => {
        this.handle('slotResponseReceived', () =>
          this.store.recordSlotResponseReceived(event.slot)
        );
      });
      pubads.addEventListener('slotRenderEnded', (event) => {
        this.handle('slotRenderEnded', () => {
          const renderEvent = event as GptRenderEvent;
          this.store.recordSlotRenderEnded(renderEvent.slot, {
            isEmpty: typeof renderEvent.isEmpty === 'boolean' ? renderEvent.isEmpty : undefined,
            size: normalizeSize(renderEvent.size),
            isBackfill:
              typeof renderEvent.isBackfill === 'boolean' ? renderEvent.isBackfill : undefined,
            slotContentChanged:
              typeof renderEvent.slotContentChanged === 'boolean'
                ? renderEvent.slotContentChanged
                : undefined,
          });
        });
      });
      pubads.addEventListener('slotOnload', (event) => {
        this.handle('slotOnload', () => this.store.recordSlotOnload(event.slot));
      });
      pubads.addEventListener('impressionViewable', (event) => {
        this.handle('impressionViewable', () => this.store.recordImpressionViewable(event.slot));
      });
      pubads.addEventListener('slotVisibilityChanged', (event) => {
        this.handle('slotVisibilityChanged', () => {
          const visibilityEvent = event as GptVisibilityEvent;
          this.store.recordSlotVisibilityChanged(
            visibilityEvent.slot,
            visibilityEvent.inViewPercentage
          );
        });
      });

      this.installed = true;
      this.store.markGptObserved();
    } catch (error) {
      this.logger.warn('gpt diagnostics: listener installation failed', error);
    }
  }

  private handle(kind: GptEventName, callback: () => void): void {
    try {
      callback();
    } catch (error) {
      this.logger.warn(`gpt diagnostics: ${kind} callback failed`, error);
    }
  }
}
