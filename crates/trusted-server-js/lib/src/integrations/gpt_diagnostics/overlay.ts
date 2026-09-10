import { log } from '../../core/log';
import type { GptDiagnosticsRequestCycle } from '../../core/types';

import type { GptDiagnosticsBindingManager } from './binding';
import { unhandledCase } from './exhaustive';
import {
  auctionTypeLabel,
  displayableGptFillSize,
  formatSizes,
  scheduleFrame,
} from './presentation_helpers';
import type { GptDiagnosticsStoreSlotSnapshot, GptDiagnosticsStoreSnapshot } from './store';

export const GPT_DIAGNOSTICS_HOST_ID = 'trusted-server-gpt-diagnostics';

export type GptDiagnosticsFilter = 'all' | 'visible' | 'filled' | 'empty' | 'pending' | 'unbound';

interface OverlayStore {
  snapshot(): GptDiagnosticsStoreSnapshot;
  subscribe(listener: () => void): () => void;
}

interface OverlayBindings {
  get: GptDiagnosticsBindingManager['get'];
  subscribe(listener: () => void): () => void;
}

type OverlayWindow = Window & {
  MutationObserver?: typeof MutationObserver;
};

interface OverlayOptions {
  window?: OverlayWindow;
  document?: Document;
  scheduleFrame?: (callback: () => void) => void;
  onExport?: () => void;
  onShadowRoot?: (root: ShadowRoot) => void;
  onBadgeLayerChange?: (layer: HTMLElement | undefined) => void;
}

const PANEL_STYLES = `
  :host { all: initial; }
  *, *::before, *::after { box-sizing: border-box; }
  .tsgd-panel {
    position: fixed;
    z-index: 2147483647;
    right: 12px;
    bottom: 12px;
    width: min(460px, calc(100vw - 24px));
    max-height: min(720px, calc(100vh - 24px));
    display: flex;
    flex-direction: column;
    overflow: hidden;
    color: #f8fafc;
    background: #111827;
    border: 1px solid #475569;
    border-radius: 10px;
    box-shadow: 0 16px 50px rgb(0 0 0 / 45%);
    font: 13px/1.4 ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    pointer-events: auto;
  }
  .tsgd-panel[hidden] { display: none; }
  .tsgd-header, .tsgd-toolbar, .tsgd-summary, .tsgd-slot { padding: 10px 12px; }
  .tsgd-header { display: flex; align-items: center; gap: 8px; border-bottom: 1px solid #334155; }
  .tsgd-title { margin: 0; flex: 1; font-size: 14px; font-weight: 700; }
  .tsgd-status { color: #93c5fd; font-size: 12px; }
  button, select {
    color: inherit;
    background: #1e293b;
    border: 1px solid #475569;
    border-radius: 5px;
    min-height: 30px;
    padding: 4px 8px;
    font: inherit;
  }
  button { cursor: pointer; }
  button:focus-visible, select:focus-visible, summary:focus-visible, a:focus-visible { outline: 2px solid #60a5fa; outline-offset: 2px; }
  .tsgd-toolbar { display: flex; gap: 8px; align-items: center; border-bottom: 1px solid #334155; }
  .tsgd-toolbar label { color: #cbd5e1; }
  .tsgd-summary { color: #cbd5e1; border-bottom: 1px solid #334155; }
  .tsgd-coverage { margin: 6px 0 0; padding: 0; list-style: none; font-size: 12px; }
  .tsgd-content { overflow: auto; overscroll-behavior: contain; }
  .tsgd-empty { padding: 18px 12px; color: #94a3b8; }
  .tsgd-slot { border-bottom: 1px solid #334155; }
  .tsgd-slot:last-child { border-bottom: 0; }
  .tsgd-slot[aria-current="true"], .tsgd-cycle[aria-current="true"] { outline: 2px solid #60a5fa; outline-offset: -2px; }
  .tsgd-group { margin-top: 8px; }
  .tsgd-group h3 { margin: 0; color: #e2e8f0; font-size: 12px; }
  .tsgd-help { padding: 0 12px 8px; color: #cbd5e1; }
  .tsgd-help p { margin: 6px 0 0; }
  .tsgd-locate { margin-top: 8px; }
  .tsgd-selection-note { padding: 8px 12px; color: #fde68a; border-bottom: 1px solid #334155; }
  .tsgd-slot-title { display: flex; gap: 8px; align-items: baseline; }
  .tsgd-slot-title strong { overflow-wrap: anywhere; }
  .tsgd-state { margin-left: auto; color: #fde68a; white-space: nowrap; }
  .tsgd-facts { margin: 6px 0 0; padding: 0; list-style: none; color: #cbd5e1; font-size: 12px; }
  .tsgd-facts li { overflow-wrap: anywhere; }
  details { margin-top: 8px; }
  summary { cursor: pointer; color: #93c5fd; }
  .tsgd-cycle { margin: 6px 0 0; padding: 6px 8px; background: #0f172a; border-radius: 5px; }
  .tsgd-badge-layer { position: fixed; z-index: 2147483646; inset: 0; pointer-events: none; }
  .tsgd-badge {
    position: fixed;
    pointer-events: auto;
    padding: 5px 7px;
    color: #fff;
    background: rgb(15 23 42 / 94%);
    border: 1px solid #60a5fa;
    border-radius: 5px;
    box-shadow: 0 2px 8px rgb(0 0 0 / 35%);
    font: 11px/1.35 ui-sans-serif, system-ui, sans-serif;
    white-space: pre-line;
    text-align: left;
    cursor: pointer;
  }
  .tsgd-highlight {
    position: fixed;
    border: 3px solid #fbbf24;
    background: rgb(251 191 36 / 18%);
    box-shadow: 0 0 0 3px rgb(15 23 42 / 75%);
    pointer-events: none;
  }
`;

function latestCycle(
  slot: GptDiagnosticsStoreSlotSnapshot
): GptDiagnosticsRequestCycle | undefined {
  return slot.requests[slot.requests.length - 1];
}

function primaryState(cycle: GptDiagnosticsRequestCycle | undefined): string {
  if (!cycle) return 'Waiting for request';
  if (cycle.isEmpty === true) return 'Empty';
  if (cycle.isEmpty === false) return 'Filled';
  if (cycle.renderAtMs !== undefined) return 'Rendered (fill unknown)';
  if (cycle.responseAtMs !== undefined) return 'Response received';
  return 'Requesting';
}

function formatMilliseconds(value: number | undefined): string | undefined {
  if (value === undefined || !Number.isFinite(value) || value < 0) return undefined;
  return `${Math.round(value * 10) / 10} ms`;
}

function deliveryFact(cycle: GptDiagnosticsRequestCycle): string {
  switch (cycle.delivery) {
    case 'trusted_server_response_sent':
      return 'Creative markup sent; execution not confirmed';
    case 'trusted_server_selected':
      return 'Server bid selected by the creative bridge; response not confirmed';
    case 'candidate_unconfirmed':
      return 'Server bid available; selection not confirmed';
    case 'no_candidate':
      return 'No direct Trusted Server candidate';
    case 'unknown':
      return 'Delivery status unknown — required evidence was not observed';
    case 'pending':
      return 'Waiting for Trusted Server creative evidence';
    case 'not_applicable':
      return 'Delivery evidence: Not applicable';
    case undefined:
      return 'Delivery evidence: Not observed';
    default:
      return unhandledCase(cycle.delivery);
  }
}

function servedBidderFact(cycle: GptDiagnosticsRequestCycle): string | undefined {
  return cycle.isEmpty === false ? 'Served bidder not confirmed' : undefined;
}

function requestPathFact(cycle: GptDiagnosticsRequestCycle): string {
  switch (cycle.requestPath) {
    case 'trusted_server_direct':
      return 'Request path: Trusted Server direct';
    case 'prebid_refresh':
      return 'Request path: Prebid refresh';
    case 'publisher_refresh':
      return 'Request path: Publisher refresh';
    case 'competing':
      return 'Request path: Multiple paths observed';
    case 'unattributed':
      return 'Request path: Not observed';
    case undefined:
      return 'Request path: Not observed';
  }
}

function trustedServerOpportunityFact(cycle: GptDiagnosticsRequestCycle): string {
  switch (cycle.trustedServerOpportunity) {
    case 'renderable_candidate':
      return 'Server bid available; creative source present';
    case 'unrenderable_candidate':
      return 'Server bid available; creative source incomplete';
    case 'no_candidate':
      return 'Direct opportunity: No candidate';
    case undefined:
      return 'Direct opportunity: Not observed';
  }
}

function creativeFailureFact(
  failure: NonNullable<GptDiagnosticsRequestCycle['trustedServerCreativeFailures']>[number]
): string {
  switch (failure) {
    case 'missing_render_source':
      return 'Creative bridge failure: missing render source';
    case 'cache_fetch_failed':
      return 'Creative bridge failure: cache fetch failed';
    case 'invalid_cache_payload':
      return 'Creative bridge failure: invalid cache payload';
    case 'response_post_failed':
      return 'Creative bridge failure: response post failed';
  }
}

function adManagerFact(cycle: GptDiagnosticsRequestCycle): string | undefined {
  const identity = cycle.adManager;
  if (!identity) return undefined;

  const parts: string[] = [];
  const lineItem = identity.lineItemId ?? identity.sourceAgnosticLineItemId;
  const creative = identity.creativeId ?? identity.sourceAgnosticCreativeId;
  if (lineItem) parts.push(`line item ${lineItem}`);
  if (identity.campaignId) parts.push(`order ${identity.campaignId}`);
  if (identity.advertiserId) parts.push(`advertiser ${identity.advertiserId}`);
  if (creative) parts.push(`creative ${creative}`);
  if (identity.yieldGroupIds?.length)
    parts.push(`yield group ${identity.yieldGroupIds.join(', ')}`);
  if (identity.companyIds?.length) parts.push(`company ${identity.companyIds.join(', ')}`);
  return parts.length > 0 ? `Ad Manager reported ${parts.join(' · ')}` : undefined;
}

function responseClassFact(cycle: GptDiagnosticsRequestCycle): string | undefined {
  switch (cycle.responseClass) {
    case 'empty':
      return 'Ad Manager response class: empty';
    case 'backfill':
      return 'Ad Manager response class: backfill';
    case 'reservation':
      return 'Ad Manager response class: reservation';
    case 'unclassified_non_empty':
      return 'Ad Manager response class: unclassified non-empty';
    case undefined:
      return undefined;
    default:
      return unhandledCase(cycle.responseClass);
  }
}

function auctionFacts(cycle: GptDiagnosticsRequestCycle): string[] {
  const facts = [
    requestPathFact(cycle),
    `Auction evidence: ${cycle.auctionType ? auctionTypeLabel(cycle.auctionType) : 'Auction not observed'}`,
  ];
  if (cycle.auctionWinner) {
    facts.push(`Server auction winner: ${cycle.auctionWinner.bidder}`);
    facts.push(
      `Server bid price bucket: ${cycle.auctionWinner.priceBucket} ${cycle.auctionWinner.currency ?? '(currency not supplied)'}`
    );
  }
  if (cycle.prebidAuction?.targetingCandidate) {
    const candidate = cycle.prebidAuction.targetingCandidate;
    facts.push(`Prebid targeting candidate: ${candidate.bidder}`);
    facts.push(
      `Prebid candidate price bucket: ${candidate.priceBucket} ${candidate.currency ?? '(currency not supplied)'}`
    );
  }
  if (cycle.prebidAuction?.win) {
    const win = cycle.prebidAuction.win;
    facts.push(`Prebid bidWon observation: ${win.bidder}`);
    facts.push(
      `Prebid win price bucket: ${win.priceBucket} ${win.currency ?? '(currency not supplied)'}`
    );
  }
  const servedBidder = servedBidderFact(cycle);
  if (servedBidder) facts.push(servedBidder);
  return facts;
}

function timingFacts(cycle: GptDiagnosticsRequestCycle): string[] {
  const serverTimingApplies =
    cycle.auctionType === 'ssat' ||
    cycle.auctionType === 'trusted_server' ||
    cycle.auctionType === 'competing';
  const missingServerTiming = serverTimingApplies ? 'Unavailable' : 'Not applicable';
  const serverTimings = [
    ['Server request start → auction dispatched', cycle.serverAuctionTimings?.auctionDispatchedMs],
    ['Server request start → auction collected', cycle.serverAuctionTimings?.auctionResolvedMs],
    ['Server request start → bids ready', cycle.serverAuctionTimings?.auctionCommittedMs],
  ] as const;
  const facts = serverTimings.map(
    ([label, timing]) => `${label} ${formatMilliseconds(timing) ?? missingServerTiming}`
  );
  const wait = formatMilliseconds(cycle.serverAuctionTimings?.auctionWaitMs);
  if (wait) {
    const placement =
      cycle.serverAuctionTimings?.auctionWaitPlacement === 'pre_header'
        ? 'pre-header'
        : cycle.serverAuctionTimings?.auctionWaitPlacement === 'in_stream'
          ? 'in stream'
          : 'placement unknown';
    facts.push(`Auction collection wait (${placement}) ${wait}`);
  } else {
    facts.push(`Auction collection wait ${missingServerTiming}`);
  }
  facts.push(
    `Opportunity → request ${formatMilliseconds(cycle.opportunityToRequestMs) ?? 'Unavailable'}`
  );
  const durations = [
    ['GAM request → response', cycle.durations.requestToResponseMs],
    ['GAM response → render', cycle.durations.responseToRenderMs],
    ['GAM request → render', cycle.durations.requestToRenderMs],
    ['Render → load', cycle.durations.renderToLoadMs],
    ['Render → viewable', cycle.durations.renderToViewableMs],
  ] as const;
  for (const [label, duration] of durations) {
    facts.push(`${label} ${formatMilliseconds(duration) ?? 'Unavailable'}`);
  }
  return facts;
}

function deliveryFacts(cycle: GptDiagnosticsRequestCycle): string[] {
  const facts = [
    deliveryFact(cycle),
    responseClassFact(cycle) ?? 'Ad Manager response class: Not observed',
    adManagerFact(cycle) ?? 'Ad Manager fields: Not observed',
  ];
  if (cycle.loadAtMs !== undefined) facts.push('GPT slot onload observed');
  if (cycle.viewableAtMs !== undefined) facts.push('GPT impressionViewable observed');
  if (cycle.incompleteSequence) facts.push('Incomplete sequence');
  if (cycle.isBackfill !== undefined) facts.push(`Backfill ${cycle.isBackfill ? 'yes' : 'no'}`);
  if (cycle.slotContentChanged !== undefined) {
    facts.push(`Slot content changed ${cycle.slotContentChanged ? 'yes' : 'no'}`);
  }
  return facts;
}

function sizeFacts(cycle: GptDiagnosticsRequestCycle): string[] {
  const fillSize = displayableGptFillSize(cycle.size);
  return [
    cycle.requestedSlotSizes
      ? `Requested sizes ${formatSizes(cycle.requestedSlotSizes)}`
      : 'Requested sizes: Not observed',
    fillSize
      ? `GPT-reported size ${formatSizes([fillSize])}`
      : cycle.size?.[0] === 1 && cycle.size[1] === 1
        ? 'GPT-reported size: 1×1 placeholder hidden'
        : 'GPT-reported size: Not observed',
    cycle.observedSlotSize
      ? `Size filled ${formatSizes([cycle.observedSlotSize])} · Measured outer slot size`
      : 'Size filled: Not observed · Measured outer slot size',
  ];
}

function technicalCycleFacts(cycle: GptDiagnosticsRequestCycle): string[] {
  const facts = [trustedServerOpportunityFact(cycle)];
  if (Number.isSafeInteger(cycle.requestIntentId) && cycle.requestIntentId! > 0) {
    facts.push(`Request intent: ${cycle.requestIntentId}`);
  }
  if (typeof cycle.trustedServerAuctionId === 'string' && cycle.trustedServerAuctionId.length > 0) {
    facts.push(`Trusted Server auction: ${cycle.trustedServerAuctionId}`);
  }
  if (cycle.prebidAuction?.auctionId) {
    facts.push(`Prebid auction: ${cycle.prebidAuction.auctionId}`);
  }
  const previousRenderToRequest = formatMilliseconds(cycle.previousRenderToRequestMs);
  if (cycle.replacedRequestNumber !== undefined && previousRenderToRequest) {
    facts.push(
      `Replaced rendered request ${cycle.replacedRequestNumber} after ${previousRenderToRequest}`
    );
  }
  if (
    cycle.creativeChanged !== undefined &&
    cycle.previousCreativeId !== undefined &&
    (cycle.adManager?.creativeId ?? cycle.adManager?.sourceAgnosticCreativeId) !== undefined
  ) {
    const currentCreativeId =
      cycle.adManager?.creativeId ?? cycle.adManager?.sourceAgnosticCreativeId;
    facts.push(
      cycle.creativeChanged
        ? `Creative changed ${cycle.previousCreativeId} → ${currentCreativeId}`
        : `Creative unchanged ${currentCreativeId}`
    );
  }
  const creativeRequestAt = formatMilliseconds(cycle.trustedServerCreativeRequestAtMs);
  if (creativeRequestAt) {
    facts.push(`Trusted Server creative request observed at ${creativeRequestAt}`);
  }
  const creativeResponseAt = formatMilliseconds(cycle.trustedServerCreativeResponseAtMs);
  if (creativeResponseAt) {
    facts.push(`Trusted Server markup response sent at ${creativeResponseAt}`);
  }
  for (const failure of new Set(cycle.trustedServerCreativeFailures ?? [])) {
    facts.push(creativeFailureFact(failure));
  }
  return facts;
}

function cycleFacts(cycle: GptDiagnosticsRequestCycle): string[] {
  return [
    ...auctionFacts(cycle),
    ...deliveryFacts(cycle),
    ...timingFacts(cycle),
    ...sizeFacts(cycle),
    ...technicalCycleFacts(cycle),
  ];
}

function cycleLabel(cycle: GptDiagnosticsRequestCycle): string {
  return cycle.requestNumber === 1 ? 'Initial request' : `Refresh ${cycle.requestNumber - 1}`;
}

function matchesFilter(
  slot: GptDiagnosticsStoreSlotSnapshot,
  bindings: OverlayBindings,
  filter: GptDiagnosticsFilter
): boolean {
  if (filter === 'all') return true;
  const binding = bindings.get(slot.runtimeSlotNumber);
  const latest = latestCycle(slot);
  if (filter === 'visible') return binding.binding.status === 'bound' && binding.visible;
  if (filter === 'filled') return latest?.isEmpty === false;
  if (filter === 'empty') return latest?.isEmpty === true;
  if (filter === 'pending')
    return !latest || latest.isEmpty === undefined || latest.incompleteSequence;
  return binding.binding.status !== 'bound';
}

function appendFacts(document: Document, parent: HTMLElement, facts: string[]): void {
  const list = document.createElement('ul');
  list.className = 'tsgd-facts';
  for (const fact of facts) {
    const item = document.createElement('li');
    item.textContent = fact;
    list.append(item);
  }
  parent.append(list);
}

function appendGroup(
  document: Document,
  parent: HTMLElement,
  heading: string,
  facts: string[]
): void {
  const section = document.createElement('section');
  section.className = 'tsgd-group';
  const title = document.createElement('h3');
  title.textContent = heading;
  section.append(title);
  appendFacts(document, section, facts);
  parent.append(section);
}

/** Owns hydration-safe mounting and the closed-shadow diagnostics panel. */
export class GptDiagnosticsOverlay {
  private readonly store: OverlayStore;
  private readonly bindings: OverlayBindings;
  private readonly window: OverlayWindow;
  private readonly document: Document;
  private readonly scheduleFrame: (callback: () => void) => void;
  private readonly onExport: () => void;
  private readonly onShadowRoot?: (root: ShadowRoot) => void;
  private readonly onBadgeLayerChange?: (layer: HTMLElement | undefined) => void;
  private readonly unsubscribeStore: () => void;
  private readonly unsubscribeBindings: () => void;
  private host?: HTMLElement;
  private panel?: HTMLElement;
  private badgeLayer?: HTMLElement;
  private lifecycleObserver?: MutationObserver;
  private visualReady = false;
  private mountWaitStarted = false;
  private renderScheduled = false;
  private remountScheduled = false;
  private hostCollision = false;
  private collapsed = false;
  private dismissed = false;
  private destroyed = false;
  private filter: GptDiagnosticsFilter = 'all';
  private selectedRequest?: { runtimeSlotNumber: number; requestNumber: number };
  private selectedRequestHasFocus = false;

  constructor(store: OverlayStore, bindings: OverlayBindings, options: OverlayOptions = {}) {
    this.store = store;
    this.bindings = bindings;
    this.window = options.window ?? (window as unknown as OverlayWindow);
    this.document = options.document ?? document;
    this.scheduleFrame =
      options.scheduleFrame ?? ((callback) => scheduleFrame(this.window, callback));
    this.onExport = options.onExport ?? (() => undefined);
    this.onShadowRoot = options.onShadowRoot;
    this.onBadgeLayerChange = options.onBadgeLayerChange;
    this.unsubscribeStore = this.store.subscribe(() => this.scheduleRender());
    this.unsubscribeBindings = this.bindings.subscribe(() => this.scheduleRender());
    this.installLifecycleObserver();
    this.beginMountWait();
  }

  show(): void {
    if (this.destroyed) return;
    this.dismissed = false;
    if (this.visualReady) this.mount();
    else this.beginMountWait();
  }

  hide(): void {
    if (this.destroyed) return;
    this.dismissed = true;
    this.removeHost();
  }

  /** Open and focus the exact retained request selected from an on-page badge. */
  selectRequest(runtimeSlotNumber: number, requestNumber: number): void {
    if (this.destroyed) return;
    this.selectedRequest = { runtimeSlotNumber, requestNumber };
    this.filter = 'all';
    this.collapsed = false;
    this.show();
    this.render();
    this.scheduleFrame(() => {
      const selected = this.panel?.querySelector<HTMLElement>(
        `[data-runtime-slot="${runtimeSlotNumber}"][data-request-number="${requestNumber}"]`
      );
      selected?.focus();
      selected?.scrollIntoView?.({ block: 'nearest' });
    });
  }

  destroy(): void {
    if (this.destroyed) return;
    this.destroyed = true;
    this.dismissed = true;
    this.unsubscribeStore();
    this.unsubscribeBindings();
    this.lifecycleObserver?.disconnect();
    this.document.removeEventListener('readystatechange', this.handleReadyStateChange);
    this.window.removeEventListener('load', this.handleReadyStateChange);
    this.removeHost();
  }

  private readonly handleReadyStateChange = (): void => {
    this.beginMountWait();
  };

  private beginMountWait(): void {
    if (this.destroyed || this.visualReady || this.mountWaitStarted) return;
    if (this.document.readyState !== 'complete') {
      this.document.addEventListener('readystatechange', this.handleReadyStateChange);
      this.window.addEventListener('load', this.handleReadyStateChange, { once: true });
      return;
    }

    this.mountWaitStarted = true;
    this.document.removeEventListener('readystatechange', this.handleReadyStateChange);
    this.scheduleFrame(() => {
      this.scheduleFrame(() => {
        this.visualReady = true;
        if (!this.dismissed) this.mount();
      });
    });
  }

  private mount(): void {
    if (this.destroyed || this.dismissed || !this.visualReady || this.host?.isConnected) return;

    const existing = this.document.getElementById(GPT_DIAGNOSTICS_HOST_ID);
    if (existing) {
      if (!this.hostCollision) {
        log.warn('gpt diagnostics: host element ID collision; panel not mounted');
      }
      this.hostCollision = true;
      return;
    }
    this.hostCollision = false;

    const host = this.document.createElement('div');
    host.id = GPT_DIAGNOSTICS_HOST_ID;
    const root = host.attachShadow({ mode: 'closed' });
    const style = this.document.createElement('style');
    style.textContent = PANEL_STYLES;
    const panel = this.document.createElement('section');
    panel.className = 'tsgd-panel';
    panel.setAttribute('role', 'region');
    panel.setAttribute('aria-label', 'GPT runtime diagnostics');
    const badgeLayer = this.document.createElement('div');
    badgeLayer.className = 'tsgd-badge-layer';
    root.append(style, badgeLayer, panel);

    this.host = host;
    this.panel = panel;
    this.badgeLayer = badgeLayer;
    (this.document.body ?? this.document.documentElement).append(host);
    this.onShadowRoot?.(root);
    this.onBadgeLayerChange?.(badgeLayer);
    this.render();
  }

  private removeHost(): void {
    this.onBadgeLayerChange?.(undefined);
    const host = this.host;
    this.host = undefined;
    this.panel = undefined;
    this.badgeLayer = undefined;
    host?.remove();
  }

  private installLifecycleObserver(): void {
    const Observer = this.window.MutationObserver;
    if (typeof Observer !== 'function' || !this.document.documentElement) return;
    this.lifecycleObserver = new Observer(() => {
      if (
        this.destroyed ||
        this.dismissed ||
        !this.visualReady ||
        this.host?.isConnected ||
        this.remountScheduled
      ) {
        return;
      }
      if (this.hostCollision) {
        if (this.document.getElementById(GPT_DIAGNOSTICS_HOST_ID)) return;
        this.hostCollision = false;
      }
      this.remountScheduled = true;
      this.scheduleFrame(() => {
        this.remountScheduled = false;
        this.mount();
      });
    });
    this.lifecycleObserver.observe(this.document.documentElement, {
      childList: true,
      subtree: true,
    });
  }

  private scheduleRender(): void {
    if (this.destroyed || this.renderScheduled) return;
    this.renderScheduled = true;
    this.scheduleFrame(() => {
      this.renderScheduled = false;
      this.render();
    });
  }

  private render(): void {
    if (!this.panel || !this.host?.isConnected) return;
    const snapshot = this.store.snapshot();
    const panel = this.panel;
    const previousContent = panel.querySelector<HTMLElement>('.tsgd-content');
    const previousScrollTop = previousContent?.scrollTop ?? 0;
    const selectedRequestWasFocused = this.selectedRequestHasFocus;
    const openHistorySlots = new Set(
      Array.from(panel.querySelectorAll<HTMLDetailsElement>('.tsgd-history[open]'))
        .map((details) => details.closest<HTMLElement>('.tsgd-slot')?.dataset.runtimeSlot)
        .filter((runtimeSlot): runtimeSlot is string => runtimeSlot !== undefined)
    );
    panel.replaceChildren();

    const header = this.document.createElement('header');
    header.className = 'tsgd-header';
    const title = this.document.createElement('h2');
    title.className = 'tsgd-title';
    title.textContent = 'GPT runtime diagnostics';
    const status = this.document.createElement('span');
    status.className = 'tsgd-status';
    status.textContent = snapshot.gptObserved ? 'GPT observed' : 'Waiting for GPT';
    const collapse = this.button(this.collapsed ? 'Expand' : 'Collapse', () => {
      this.collapsed = !this.collapsed;
      this.render();
    });
    collapse.setAttribute('aria-expanded', String(!this.collapsed));
    const close = this.button('Close', () => this.hide());
    header.append(title, status, collapse, close);
    panel.append(header);

    if (this.collapsed) return;

    const toolbar = this.document.createElement('div');
    toolbar.className = 'tsgd-toolbar';
    const filterLabel = this.document.createElement('label');
    filterLabel.textContent = 'Filter';
    const select = this.document.createElement('select');
    select.setAttribute('aria-label', 'Filter diagnostic slots');
    const filters: Array<[GptDiagnosticsFilter, string]> = [
      ['all', 'All'],
      ['visible', 'Visible'],
      ['filled', 'Filled'],
      ['empty', 'Empty'],
      ['pending', 'Pending/Incomplete'],
      ['unbound', 'Unbound/Ambiguous'],
    ];
    for (const [value, label] of filters) {
      const option = this.document.createElement('option');
      option.value = value;
      option.textContent = label;
      option.selected = this.filter === value;
      select.append(option);
    }
    select.addEventListener('change', () => {
      this.filter = select.value as GptDiagnosticsFilter;
      this.render();
    });
    const exportButton = this.button('Export JSON', () => this.onExport());
    const dictionaryLink = this.document.createElement('a');
    dictionaryLink.href =
      'https://iabtechlab.github.io/trusted-server/guide/integrations/gpt-diagnostics-dictionary';
    dictionaryLink.target = '_blank';
    dictionaryLink.rel = 'noopener';
    dictionaryLink.textContent = 'Label dictionary';
    dictionaryLink.setAttribute('aria-label', 'Open GPT diagnostics label dictionary');
    filterLabel.append(select);
    toolbar.append(filterLabel, exportButton, dictionaryLink);
    panel.append(toolbar);

    const help = this.document.createElement('details');
    help.className = 'tsgd-help';
    const helpSummary = this.document.createElement('summary');
    helpSummary.textContent = 'How to read this evidence';
    const helpText = this.document.createElement('p');
    helpText.textContent =
      'Auction winners, Prebid candidates, and GPT render results are separate observations. “Filled” does not identify the served bidder. Browser and server timings use separate clocks.';
    help.append(helpSummary, helpText);
    panel.append(help);

    const summary = this.document.createElement('div');
    summary.className = 'tsgd-summary';
    summary.textContent = `${snapshot.slots.length} slots · ${snapshot.callbackIssues.length} callback issues · ${snapshot.attributionIssues?.length ?? 0} attribution issues`;
    const coverage = this.document.createElement('ul');
    coverage.className = 'tsgd-coverage';
    for (const [kind, counters] of Object.entries(snapshot.coverage)) {
      if (counters.observed === 0) continue;
      const item = this.document.createElement('li');
      item.textContent = `${kind}: ${counters.observed} observed · ${counters.matched} matched · ${counters.unmatched} unmatched · ${counters.ambiguous} ambiguous`;
      coverage.append(item);
    }
    summary.append(coverage);
    panel.append(summary);

    if (
      this.selectedRequest &&
      !snapshot.slots.some(
        (slot) =>
          slot.runtimeSlotNumber === this.selectedRequest?.runtimeSlotNumber &&
          slot.requests.some((cycle) => cycle.requestNumber === this.selectedRequest?.requestNumber)
      )
    ) {
      const note = this.document.createElement('div');
      note.className = 'tsgd-selection-note';
      note.setAttribute('role', 'status');
      note.textContent = `Ad #${this.selectedRequest.runtimeSlotNumber}, Request #${this.selectedRequest.requestNumber} is no longer retained.`;
      panel.append(note);
    }

    const content = this.document.createElement('div');
    content.className = 'tsgd-content';
    const filteredSlots = snapshot.slots.filter((slot) =>
      matchesFilter(slot, this.bindings, this.filter)
    );
    if (filteredSlots.length === 0) {
      const empty = this.document.createElement('div');
      empty.className = 'tsgd-empty';
      empty.textContent =
        snapshot.slots.length === 0 ? 'No GPT slots observed yet.' : 'No slots match.';
      content.append(empty);
    } else {
      for (const slot of filteredSlots) {
        const selectedPreviousRequest =
          this.selectedRequest?.runtimeSlotNumber === slot.runtimeSlotNumber &&
          this.selectedRequest.requestNumber !== latestCycle(slot)?.requestNumber;
        content.append(
          this.renderSlot(
            slot,
            openHistorySlots.has(String(slot.runtimeSlotNumber)) || selectedPreviousRequest
          )
        );
      }
    }
    panel.append(content);
    content.scrollTop = previousScrollTop;
    if (selectedRequestWasFocused) {
      panel.querySelector<HTMLElement>('[aria-current="true"]')?.focus({ preventScroll: true });
    }
  }

  private renderSlot(slot: GptDiagnosticsStoreSlotSnapshot, historyOpen: boolean): HTMLElement {
    const container = this.document.createElement('article');
    container.className = 'tsgd-slot';
    const latest = latestCycle(slot);
    container.dataset.runtimeSlot = String(slot.runtimeSlotNumber);
    if (latest) container.dataset.requestNumber = String(latest.requestNumber);
    const latestSelected =
      latest !== undefined &&
      this.selectedRequest?.runtimeSlotNumber === slot.runtimeSlotNumber &&
      this.selectedRequest.requestNumber === latest.requestNumber;
    if (latestSelected) {
      container.tabIndex = -1;
      container.setAttribute('aria-current', 'true');
      this.trackSelectedRequestFocus(container);
    }

    const title = this.document.createElement('div');
    title.className = 'tsgd-slot-title';
    const name = this.document.createElement('strong');
    name.textContent = `Ad #${slot.runtimeSlotNumber}${latest ? ` · Request #${latest.requestNumber}` : ''} · ${slot.slotElementId ?? 'Unbound GPT slot'}`;
    const state = this.document.createElement('span');
    state.className = 'tsgd-state';
    state.textContent = primaryState(latest);
    title.append(name, state);
    container.append(title);

    const binding = this.bindings.get(slot.runtimeSlotNumber);
    if (binding.binding.status === 'bound' && binding.element?.isConnected) {
      const locate = this.button('Locate on page', () => this.locateOnPage(slot.runtimeSlotNumber));
      locate.className = 'tsgd-locate';
      container.append(locate);
    }

    if (latest) {
      const summaryFacts = [
        `Ad #${slot.runtimeSlotNumber} · Request #${latest.requestNumber}`,
        `GPT result: ${primaryState(latest)}`,
        `Observed auction path: ${latest.auctionType ? auctionTypeLabel(latest.auctionType) : 'Auction not observed'}`,
        deliveryFact(latest),
      ];
      const servedBidder = servedBidderFact(latest);
      if (servedBidder) summaryFacts.push(servedBidder);
      appendGroup(this.document, container, 'Summary', summaryFacts);
      appendGroup(this.document, container, 'Auction evidence', auctionFacts(latest));
      appendGroup(this.document, container, 'Delivery evidence', deliveryFacts(latest));
      appendGroup(this.document, container, 'Timing', timingFacts(latest));
      appendGroup(this.document, container, 'Size and visibility', [
        ...sizeFacts(latest),
        binding.binding.status === 'bound'
          ? `Binding: Bound · ${binding.visible ? 'Visible' : 'Outside viewport'}`
          : `Binding: ${binding.binding.status} · ${binding.binding.reason ?? 'reason unavailable'}`,
        slot.currentVisibilityPercentage !== undefined
          ? `GPT visibility ${slot.currentVisibilityPercentage}% (maximum ${slot.maximumVisibilityPercentage ?? slot.currentVisibilityPercentage}%)`
          : 'GPT visibility: Not observed',
      ]);
    }

    if (slot.requests.length > 1) {
      const history = this.document.createElement('details');
      history.className = 'tsgd-history';
      history.open = historyOpen;
      const summary = this.document.createElement('summary');
      summary.textContent = `Request history (${slot.requests.length - 1} previous)`;
      history.append(summary);
      for (const cycle of slot.requests.slice(0, -1).reverse()) {
        const previous = this.document.createElement('div');
        previous.className = 'tsgd-cycle';
        previous.dataset.runtimeSlot = String(slot.runtimeSlotNumber);
        previous.dataset.requestNumber = String(cycle.requestNumber);
        const selected =
          this.selectedRequest?.runtimeSlotNumber === slot.runtimeSlotNumber &&
          this.selectedRequest.requestNumber === cycle.requestNumber;
        if (selected) {
          previous.tabIndex = -1;
          previous.setAttribute('aria-current', 'true');
          this.trackSelectedRequestFocus(previous);
        }
        const heading = this.document.createElement('strong');
        heading.textContent = `Request #${cycle.requestNumber} · ${cycleLabel(cycle)} · ${primaryState(cycle)}`;
        previous.append(heading);
        appendFacts(this.document, previous, cycleFacts(cycle));
        history.append(previous);
      }
      container.append(history);
    }

    const technical = this.document.createElement('details');
    const technicalSummary = this.document.createElement('summary');
    technicalSummary.textContent = 'Technical details';
    technical.append(technicalSummary);
    appendFacts(this.document, technical, [
      slot.adUnitPath ? `Ad unit ${slot.adUnitPath}` : 'Ad unit: Unavailable',
      binding.binding.status === 'bound'
        ? `Bound · ${binding.visible ? 'Visible' : 'Outside viewport'}`
        : binding.binding.status === 'ambiguous'
          ? `Ambiguous binding · ${binding.binding.reason ?? 'reason unavailable'}`
          : `Unbound · ${binding.binding.reason ?? 'reason unavailable'}`,
      ...(latest ? technicalCycleFacts(latest) : []),
    ]);
    container.append(technical);
    return container;
  }

  private trackSelectedRequestFocus(element: HTMLElement): void {
    element.addEventListener('focus', () => {
      this.selectedRequestHasFocus = true;
    });
    element.addEventListener('blur', () => {
      this.selectedRequestHasFocus = false;
    });
  }

  private locateOnPage(runtimeSlotNumber: number): void {
    const binding = this.bindings.get(runtimeSlotNumber);
    const element = binding.element;
    if (binding.binding.status !== 'bound' || !element?.isConnected) return;
    element.scrollIntoView({ behavior: 'auto', block: 'center', inline: 'nearest' });
    this.scheduleFrame(() => {
      if (!this.badgeLayer?.isConnected || !element.isConnected) return;
      const rectangle = element.getBoundingClientRect();
      const highlight = this.document.createElement('div');
      highlight.className = 'tsgd-highlight';
      highlight.setAttribute('aria-hidden', 'true');
      highlight.style.left = `${rectangle.left}px`;
      highlight.style.top = `${rectangle.top}px`;
      highlight.style.width = `${rectangle.width}px`;
      highlight.style.height = `${rectangle.height}px`;
      this.badgeLayer.append(highlight);
      this.window.setTimeout(() => highlight.remove(), 1500);
    });
  }

  private button(label: string, action: () => void): HTMLButtonElement {
    const button = this.document.createElement('button');
    button.type = 'button';
    button.textContent = label;
    button.setAttribute('aria-label', label);
    button.addEventListener('click', action);
    return button;
  }
}
