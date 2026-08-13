import { log } from '../../core/log';
import type { GptDiagnosticsRequestCycle } from '../../core/types';

import type { GptDiagnosticsBindingManager } from './binding';
import { unhandledCase } from './exhaustive';
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
  button:focus-visible, select:focus-visible, summary:focus-visible { outline: 2px solid #60a5fa; outline-offset: 2px; }
  .tsgd-toolbar { display: flex; gap: 8px; align-items: center; border-bottom: 1px solid #334155; }
  .tsgd-toolbar label { color: #cbd5e1; }
  .tsgd-summary { color: #cbd5e1; border-bottom: 1px solid #334155; }
  .tsgd-coverage { margin: 6px 0 0; padding: 0; list-style: none; font-size: 12px; }
  .tsgd-content { overflow: auto; overscroll-behavior: contain; }
  .tsgd-empty { padding: 18px 12px; color: #94a3b8; }
  .tsgd-slot { border-bottom: 1px solid #334155; }
  .tsgd-slot:last-child { border-bottom: 0; }
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
    padding: 5px 7px;
    color: #fff;
    background: rgb(15 23 42 / 94%);
    border: 1px solid #60a5fa;
    border-radius: 5px;
    box-shadow: 0 2px 8px rgb(0 0 0 / 35%);
    font: 11px/1.35 ui-sans-serif, system-ui, sans-serif;
    white-space: pre-line;
  }
`;

function defaultScheduleFrame(callback: () => void): void {
  if (typeof requestAnimationFrame === 'function') {
    requestAnimationFrame(() => callback());
  } else {
    queueMicrotask(callback);
  }
}

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

function deliveryFact(cycle: GptDiagnosticsRequestCycle): string | undefined {
  switch (cycle.delivery) {
    case 'trusted_server_response_sent':
      return 'Trusted Server selected; markup response sent to PUC';
    case 'trusted_server_selected':
      return 'Trusted Server selected; no markup response confirmed';
    case 'candidate_unconfirmed':
      return 'Trusted Server candidate unconfirmed — another GAM result or a creative/bridge failure is possible';
    case 'no_candidate':
      return 'adInit observed no direct Trusted Server candidate for this request';
    case 'unknown':
      return 'Delivery status unknown — required GPT or direct-candidate evidence was not observed';
    case 'pending':
      return 'Waiting for Trusted Server creative evidence';
    case 'not_applicable':
    case undefined:
      return undefined;
    default:
      return unhandledCase(cycle.delivery);
  }
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
      return 'Request path: Competing paths';
    case 'unattributed':
      return 'Request path: Unattributed';
    case undefined:
      return 'Request path: Unknown (not observed)';
  }
}

function trustedServerOpportunityFact(cycle: GptDiagnosticsRequestCycle): string {
  switch (cycle.trustedServerOpportunity) {
    case 'renderable_candidate':
      return 'Direct opportunity: Renderable candidate';
    case 'unrenderable_candidate':
      return 'Direct opportunity: Unrenderable candidate';
    case 'no_candidate':
      return 'Direct opportunity: No candidate';
    case undefined:
      return 'Direct opportunity: Unknown (not observed)';
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

function cycleFacts(cycle: GptDiagnosticsRequestCycle): string[] {
  const facts: string[] = [requestPathFact(cycle), trustedServerOpportunityFact(cycle)];
  if (Number.isSafeInteger(cycle.requestIntentId) && cycle.requestIntentId! > 0) {
    facts.push(`Request intent: ${cycle.requestIntentId}`);
  }
  if (typeof cycle.trustedServerAuctionId === 'string' && cycle.trustedServerAuctionId.length > 0) {
    facts.push(`Trusted Server auction: ${cycle.trustedServerAuctionId}`);
  }
  const opportunityToRequest = formatMilliseconds(cycle.opportunityToRequestMs);
  if (opportunityToRequest) facts.push(`Opportunity → request ${opportunityToRequest}`);
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
  const deliveryLine = deliveryFact(cycle);
  if (deliveryLine) facts.push(deliveryLine);
  const responseClassLine = responseClassFact(cycle);
  if (responseClassLine) facts.push(responseClassLine);
  const adManagerLine = adManagerFact(cycle);
  if (adManagerLine) facts.push(adManagerLine);
  if (cycle.loadAtMs !== undefined) facts.push('GPT slot onload observed');
  if (cycle.viewableAtMs !== undefined) facts.push('GPT impressionViewable observed');
  if (cycle.incompleteSequence) facts.push('Incomplete sequence');
  if (cycle.size) facts.push(`GPT reported size ${cycle.size[0]}×${cycle.size[1]}`);
  if (cycle.observedSlotSize) {
    facts.push(`Observed slot box ${cycle.observedSlotSize[0]}×${cycle.observedSlotSize[1]}`);
  }
  if (cycle.isBackfill !== undefined) facts.push(`Backfill ${cycle.isBackfill ? 'yes' : 'no'}`);
  if (cycle.slotContentChanged !== undefined) {
    facts.push(`Slot content changed ${cycle.slotContentChanged ? 'yes' : 'no'}`);
  }

  const durations = [
    ['Request → response', cycle.durations.requestToResponseMs],
    ['Response → render', cycle.durations.responseToRenderMs],
    ['Request → render', cycle.durations.requestToRenderMs],
    ['Render → load', cycle.durations.renderToLoadMs],
    ['Render → viewable', cycle.durations.renderToViewableMs],
  ] as const;
  for (const [label, duration] of durations) {
    const formatted = formatMilliseconds(duration);
    if (formatted) facts.push(`${label} ${formatted}`);
  }
  return facts;
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

  constructor(store: OverlayStore, bindings: OverlayBindings, options: OverlayOptions = {}) {
    this.store = store;
    this.bindings = bindings;
    this.window = options.window ?? (window as unknown as OverlayWindow);
    this.document = options.document ?? document;
    this.scheduleFrame = options.scheduleFrame ?? defaultScheduleFrame;
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
    badgeLayer.setAttribute('aria-hidden', 'true');
    root.append(style, badgeLayer, panel);

    this.host = host;
    this.panel = panel;
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
    const openHistorySlots = new Set(
      Array.from(panel.querySelectorAll<HTMLDetailsElement>('.tsgd-slot details[open]'))
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
    filterLabel.append(select);
    toolbar.append(filterLabel, exportButton);
    panel.append(toolbar);

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
        content.append(this.renderSlot(slot, openHistorySlots.has(String(slot.runtimeSlotNumber))));
      }
    }
    panel.append(content);
    content.scrollTop = previousScrollTop;
  }

  private renderSlot(slot: GptDiagnosticsStoreSlotSnapshot, historyOpen: boolean): HTMLElement {
    const container = this.document.createElement('article');
    container.className = 'tsgd-slot';
    container.dataset.runtimeSlot = String(slot.runtimeSlotNumber);
    const title = this.document.createElement('div');
    title.className = 'tsgd-slot-title';
    const name = this.document.createElement('strong');
    name.textContent = slot.slotElementId ?? `Unbound GPT slot ${slot.runtimeSlotNumber}`;
    const latest = latestCycle(slot);
    const state = this.document.createElement('span');
    state.className = 'tsgd-state';
    state.textContent = primaryState(latest);
    title.append(name, state);
    container.append(title);

    const binding = this.bindings.get(slot.runtimeSlotNumber);
    const facts = [
      slot.adUnitPath ? `Ad unit ${slot.adUnitPath}` : undefined,
      binding.binding.status === 'bound'
        ? `Bound · ${binding.visible ? 'Visible' : 'Outside viewport'}`
        : binding.binding.status === 'ambiguous'
          ? `Ambiguous binding · ${binding.binding.reason ?? 'unknown'}`
          : `Unbound · ${binding.binding.reason ?? 'unknown'}`,
      slot.currentVisibilityPercentage !== undefined
        ? `GPT visibility ${slot.currentVisibilityPercentage}% (maximum ${slot.maximumVisibilityPercentage ?? slot.currentVisibilityPercentage}%)`
        : undefined,
      latest ? cycleLabel(latest) : undefined,
      ...(latest ? cycleFacts(latest) : []),
    ].filter((fact): fact is string => fact !== undefined);
    appendFacts(this.document, container, facts);

    if (slot.requests.length > 1) {
      const history = this.document.createElement('details');
      history.open = historyOpen;
      const summary = this.document.createElement('summary');
      summary.textContent = `Previous requests (${slot.requests.length - 1})`;
      history.append(summary);
      for (const cycle of slot.requests.slice(0, -1).reverse()) {
        const previous = this.document.createElement('div');
        previous.className = 'tsgd-cycle';
        const heading = this.document.createElement('strong');
        heading.textContent = `${cycleLabel(cycle)} · ${primaryState(cycle)}`;
        previous.append(heading);
        appendFacts(this.document, previous, cycleFacts(cycle));
        history.append(previous);
      }
      container.append(history);
    }
    return container;
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
