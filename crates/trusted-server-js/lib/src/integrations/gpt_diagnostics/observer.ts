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

interface GptEvent {
  slot: GptDiagnosticsSlotLike;
}

interface GptRenderEvent extends GptEvent {
  isEmpty?: boolean;
  size?: unknown;
  isBackfill?: boolean;
  slotContentChanged?: boolean;
  lineItemId?: unknown;
  creativeId?: unknown;
  campaignId?: unknown;
  advertiserId?: unknown;
  sourceAgnosticLineItemId?: unknown;
  sourceAgnosticCreativeId?: unknown;
  yieldGroupIds?: unknown;
  companyIds?: unknown;
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
  refresh?: (...args: unknown[]) => unknown;
  getSlots?: () => unknown;
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
  tsjs?: Pick<TsjsApi, 'adInitRefreshInProgress' | 'prebidRefreshDispatchInProgress'>;
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

const MAX_AD_MANAGER_ID_LIST = 8;
const REFRESH_OBSERVER_INSTALLED = Symbol('gptDiagnosticsRefreshObserverInstalled');

function normalizeAdManagerId(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isSafeInteger(value) && value > 0 ? value : undefined;
}

function normalizeAdManagerIdList(value: unknown): number[] | undefined {
  if (!Array.isArray(value)) return undefined;

  const ids = value
    .map(normalizeAdManagerId)
    .filter((id): id is number => id !== undefined)
    .slice(0, MAX_AD_MANAGER_ID_LIST);
  return ids.length > 0 ? ids : undefined;
}

/**
 * Collects the Ad Manager identifiers `slotRenderEnded` exposes.
 *
 * GPT reports these only for reservation and backfill ads served by
 * PubAdsService, so an absent value is itself a fact about the render rather
 * than a gap in observation.
 */
function normalizeAdManagerIdentity(
  event: GptRenderEvent
): GptDiagnosticsAdManagerIdentity | undefined {
  const identity: GptDiagnosticsAdManagerIdentity = {
    lineItemId: normalizeAdManagerId(event.lineItemId),
    creativeId: normalizeAdManagerId(event.creativeId),
    campaignId: normalizeAdManagerId(event.campaignId),
    advertiserId: normalizeAdManagerId(event.advertiserId),
    sourceAgnosticLineItemId: normalizeAdManagerId(event.sourceAgnosticLineItemId),
    sourceAgnosticCreativeId: normalizeAdManagerId(event.sourceAgnosticCreativeId),
    yieldGroupIds: normalizeAdManagerIdList(event.yieldGroupIds),
    companyIds: normalizeAdManagerIdList(event.companyIds),
  };

  for (const key of Object.keys(identity) as Array<keyof GptDiagnosticsAdManagerIdentity>) {
    if (identity[key] === undefined) delete identity[key];
  }
  return Object.keys(identity).length > 0 ? identity : undefined;
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
            adManager: normalizeAdManagerIdentity(renderEvent),
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

      try {
        this.installRefreshObserver(pubads);
      } catch (error) {
        this.logger.warn('gpt diagnostics: refresh observer installation failed', error);
      }

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
