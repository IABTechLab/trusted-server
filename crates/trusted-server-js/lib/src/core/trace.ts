// Render-trace registry, DOM markers, and a floating debug panel: joins a
// creative rendered on the page back to the winning server-side auction bid.
import { log } from './log';
import type {
  LegacyTsjsApi,
  RenderRecord,
  RenderTraceDiagnostics,
  RenderTraceRecord,
} from './types';

/** CustomEvent fired on window after each render-trace record is written. */
export const RENDER_EVENT_NAME = 'tsjs:adRendered';

const TRACE_COOKIE_NAME = 'ts-trace';

/** DOM id of the floating trace panel (body-level overlay). */
export const TRACE_PANEL_ID = 'ts-render-trace-panel';

/**
 * Upper bound on `window.tsjs.renderLog`. A publisher page that refreshes its
 * slots on every render can produce hundreds of entries in a session, so the
 * history is trimmed from the front rather than growing without limit.
 */
const MAX_RENDER_LOG_ENTRIES = 200;
let fallbackRenderSeq = 0;

function nextRenderSeq(): number {
  try {
    const ts = (window.tsjs ??= {} as LegacyTsjsApi);
    const next = Math.max(ts.renderSeq ?? 0, fallbackRenderSeq) + 1;
    ts.renderSeq = next;
    fallbackRenderSeq = next;
    return next;
  } catch {
    return ++fallbackRenderSeq;
  }
}

/** CSS class of the per-slot confirmation badge (only on honestly-ok slots). */
export const TRACE_BADGE_CLASS = 'ts-render-badge';

/**
 * Whether the visible trace overlay is armed (`ts-trace=1` cookie present —
 * set via `GET /_ts/trace`, cleared via `GET /_ts/trace?enabled=false`).
 */
export function traceOverlayEnabled(): boolean {
  try {
    return new RegExp(`(?:^|;\\s*)${TRACE_COOKIE_NAME}=1(?:;|$)`).test(document.cookie);
  } catch {
    return false;
  }
}

/** Short-form mechanism suffix — only the bridge mechanisms add information. */
function mechanismSuffix(record: RenderRecord): string {
  return record.servedFrom === 'debug-adm' || record.servedFrom === 'pbs-cache'
    ? ` (${record.servedFrom})`
    : '';
}

/**
 * Whether an element is effectively visible: connected, non-zero box, and no
 * ancestor hiding it via `display:none`, `visibility:hidden`, or `opacity:0`.
 *
 * The ancestor walk is what catches a slot the publisher holds at `opacity:0`
 * on a wrapper until its own ad code reveals it — the slot's own computed
 * opacity is `1`, so only walking up exposes the gate.
 */
export function isEffectivelyVisible(el: Element | null): boolean {
  try {
    if (!el || !(el instanceof HTMLElement) || !el.isConnected) return false;
    const rect = el.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return false;
    let node: HTMLElement | null = el;
    while (node) {
      const cs = getComputedStyle(node);
      if (
        cs.display === 'none' ||
        cs.visibility === 'hidden' ||
        parseFloat(cs.opacity || '1') === 0
      ) {
        return false;
      }
      node = node.parentElement;
    }
    return true;
  } catch {
    return false;
  }
}

/**
 * Honest per-slot status for the panel, derived from the separate signals:
 * - `empty`   — GAM reported the slot empty, or nothing was placed.
 * - `hidden`  — a creative rendered but the slot is not visible (reveal gate).
 * - `gam-only`— GAM rendered something, but TS did not place it (can't confirm
 *               it is the TS creative — cross-origin).
 * - `ok`      — TS placed a creative and the slot is visible.
 */
type PanelStatus = 'ok' | 'hidden' | 'gam-only' | 'empty';

function panelStatus(record: RenderRecord): PanelStatus {
  if (!record.rendered || record.gamEmpty === true) return 'empty';
  if (record.visible !== true) return 'hidden';
  // `ok` requires a *confirmed* TS placement. Anything else — TS applied
  // targeting only (injected false, creative is GAM's and cross-origin
  // unreadable), or a path that never reported placement (undefined) — must not
  // be claimed as a TS render. Defaulting to gam-only keeps the panel honest
  // even if a future render path forgets to set `injected`.
  if (record.injected !== true) return 'gam-only';
  return 'ok';
}

const STATUS_STYLE: Record<PanelStatus, { color: string; mark: string; label: string }> = {
  ok: { color: '#3fb950', mark: '✓', label: 'ok' },
  hidden: { color: '#d29922', mark: '⚠', label: 'hidden' },
  'gam-only': { color: '#58a6ff', mark: '◐', label: 'gam-only' },
  empty: { color: '#f85149', mark: '✗', label: 'empty' },
};

/**
 * Attach (or replace) the per-slot confirmation badge on a slot element.
 *
 * Only called for `ok` slots — a TS creative that actually placed and is
 * visible — so the green badge on a physical banner is a truthful "this banner
 * is the render in the trace panel" marker, not the overclaiming badge the
 * first cut shipped. Hidden / gam-only / empty slots deliberately get none.
 *
 * `pointer-events: none` keeps the badge from intercepting clicks on the ad.
 */
function attachTraceBadge(el: HTMLElement, record: RenderRecord): void {
  const style = STATUS_STYLE[panelStatus(record)];

  const position = getComputedStyle(el).position;
  if (position === 'static' || position === '') {
    el.style.position = 'relative';
  }

  const badge = document.createElement('div');
  badge.className = TRACE_BADGE_CLASS;
  // Lead with the sequence number: it is what ties this badge to a panel row.
  badge.textContent =
    `TS ${style.mark} #${record.seq}` +
    `${record.bidder ? ` · ${record.bidder}` : ''}` +
    `${style.label === 'ok' ? '' : ` · ${style.label}`}`;
  badge.title = [
    `render: #${record.seq}`,
    `slot: ${record.slotId}`,
    `auction: ${record.auctionId ?? '—'}`,
    `bidder: ${record.bidder ?? '—'}`,
    `bid_id: ${record.bidId ?? '—'}`,
    `creative: ${record.creativeId ?? '—'}`,
    `adm_hash: ${record.admHash ?? '—'}`,
    `served: ${record.servedFrom ?? '—'}`,
  ].join('\n');
  const s = badge.style;
  s.setProperty('position', 'absolute');
  s.setProperty('top', '4px');
  s.setProperty('left', '4px');
  s.setProperty('z-index', '2147483646');
  s.setProperty('pointer-events', 'none');
  s.setProperty('font', '10px/1.5 ui-monospace, Menlo, Consolas, monospace');
  s.setProperty('padding', '1px 5px');
  s.setProperty('color', '#fff');
  s.setProperty('background', style.color);
  s.setProperty('border-radius', '3px');
  el.appendChild(badge);
}

/**
 * Remove this element's own trace badge, if it has one.
 *
 * Must run on *every* stamp, not only the ones that go on to attach a new
 * badge: a slot that re-renders into `empty` or `hidden` gets no replacement
 * badge, so without an unconditional removal it would keep displaying the green
 * or blue badge from its previous render — contradicting the status the panel
 * shows for the same slot.
 */
function removeTraceBadge(el: HTMLElement): void {
  el.querySelectorAll(`:scope > .${TRACE_BADGE_CLASS}`).forEach((n) => n.remove());
}

/** Truncate a long id for the compact panel row while keeping the tail. */
function short(value: string | undefined, keep = 10): string {
  if (!value) return '?';
  return value.length > keep ? `…${value.slice(-keep)}` : value;
}

/**
 * Create (or return) the floating trace panel appended to `document.body`.
 *
 * A body-level fixed overlay is used deliberately instead of per-slot badges:
 * it survives GAM/APS clearing a slot's `innerHTML`, publisher reveal gates
 * that hold a slot wrapper at `opacity: 0`, and cross-origin creative iframes —
 * none of which a child-of-slot badge can survive.
 */
function ensureTracePanel(): HTMLElement | null {
  if (typeof document === 'undefined' || !document.body) return null;

  const existing = document.getElementById(TRACE_PANEL_ID);
  if (existing) return existing;

  const panel = document.createElement('div');
  panel.id = TRACE_PANEL_ID;
  const s = panel.style;
  s.setProperty('position', 'fixed');
  s.setProperty('bottom', '12px');
  s.setProperty('right', '12px');
  s.setProperty('z-index', '2147483647');
  s.setProperty('max-width', '360px');
  s.setProperty('max-height', '45vh');
  s.setProperty('overflow', 'auto');
  s.setProperty('background', 'rgba(17,17,17,0.94)');
  s.setProperty('color', '#eee');
  s.setProperty('font', '11px/1.5 ui-monospace, Menlo, Consolas, monospace');
  s.setProperty('border', '1px solid #333');
  s.setProperty('border-radius', '6px');
  s.setProperty('box-shadow', '0 4px 16px rgba(0,0,0,0.4)');
  s.setProperty('padding', '0');
  document.body.appendChild(panel);
  return panel;
}

/**
 * Whether this record is still the live render for its slot — i.e. the entry
 * `window.tsjs.renders` currently holds. Every other row in the log has been
 * superseded by a later render of the same slot.
 *
 * Compares by object identity, not by `seq`: the registry and the history hold
 * the same record objects, so identity is exact regardless of how sequence
 * numbers were allocated.
 */
function isCurrentRender(record: RenderRecord): boolean {
  try {
    return window.tsjs?.renders?.[record.slotId] === record;
  } catch {
    return false;
  }
}

/** GAM/injection state summary for the panel's detail line. */
function stateSummary(record: RenderRecord): string {
  const parts: string[] = [];
  // GAM's own fill signal, on every render path that has one. Gating this on
  // `ssat` would hide it for `gam-refresh`, where "did GAM fill it this time"
  // is the whole question.
  if (record.gamEmpty !== undefined) {
    parts.push(`gam:${record.gamEmpty ? 'empty' : 'filled'}`);
  }
  if (record.injected !== undefined) {
    parts.push(`inj:${record.injected ? 'y' : 'n'}`);
  }
  parts.push(`vis:${record.visible === false ? 'n' : record.visible ? 'y' : '?'}`);
  return parts.join(' · ');
}

/**
 * Copy a record's full JSON to the clipboard and log it — used by the panel's
 * click-to-copy so full (untruncated) auction IDs and hashes are debuggable
 * without hovering the title or digging in `window.tsjs.renders`.
 */
function copyRecord(record: RenderRecord): void {
  const json = JSON.stringify(record, null, 2);
  log.info('trace: render record', record);
  try {
    void navigator.clipboard?.writeText(json);
  } catch {
    // Clipboard unavailable (insecure context / permissions) — the console
    // log above is the fallback.
  }
}

/** Build one slot row for the panel. */
function buildPanelRow(record: RenderRecord): HTMLElement {
  const status = panelStatus(record);
  const style = STATUS_STYLE[status];

  const row = document.createElement('div');
  const rs = row.style;
  rs.setProperty('padding', '6px 10px');
  rs.setProperty('border-top', '1px solid #2a2a2a');
  rs.setProperty('border-left', `3px solid ${style.color}`);
  rs.setProperty('cursor', 'pointer');
  // Click a row to copy its full record (untruncated IDs/hash) + log it.
  row.addEventListener('click', () => copyRecord(record));
  row.title = [
    `render: #${record.seq}`,
    `slot: ${record.slotId}`,
    `status: ${style.label}`,
    `path: ${record.path}`,
    `rendered (gam non-empty): ${record.rendered}`,
    `gam_empty: ${record.gamEmpty ?? '—'}`,
    `injected (ts placed): ${record.injected ?? '—'}`,
    `visible: ${record.visible ?? '—'}`,
    `auction: ${record.auctionId ?? '—'}`,
    `bidder: ${record.bidder ?? '—'}`,
    `creative: ${record.creativeId ?? '—'}`,
    `ad_id: ${record.adId ?? '—'}`,
    `bid_id: ${record.bidId ?? '—'}`,
    `adm_hash: ${record.admHash ?? '—'}`,
    `served: ${record.servedFrom ?? '—'}`,
    `element: ${record.elementId ?? '—'}`,
    `renders: ${record.count}`,
  ].join('\n');

  const line1 = document.createElement('div');
  const clock = new Date(record.at).toLocaleTimeString('en-GB', { hour12: false });
  // `current` marks the row still on screen for its slot — the one whose badge,
  // if any, is the badge you are looking at. Older rows are history.
  const current = isCurrentRender(record) ? ' ◂ current' : '';
  line1.textContent = `#${record.seq} ${clock} ${style.mark} ${record.slotId} · ${style.label}${current}`;
  line1.style.setProperty('font-weight', '600');
  line1.style.setProperty('color', style.color);

  const line2 = document.createElement('div');
  line2.style.setProperty('color', '#bbb');
  // An unattributed render (a GAM refresh TS ran no auction for) carries no
  // bidder or hash by design. Say that, rather than rendering `? · ?` as if a
  // lookup had failed.
  const attribution =
    record.bidder || record.admHash
      ? `${record.bidder ?? '?'} · ${short(record.admHash)}`
      : 'no TS attribution';
  line2.textContent = `${record.path}${mechanismSuffix(record)} · ${attribution}`;

  const line3 = document.createElement('div');
  line3.style.setProperty('color', '#777');
  const auction = record.auctionId ? ` · auction ${short(record.auctionId)}` : '';
  // `×N` is this slot's own render count — distinct from the page-global `#seq`
  // on line 1, which is what the on-creative badge shows.
  line3.textContent = `${stateSummary(record)}${auction} · ×${record.count}`;

  row.append(line1, line2, line3);
  return row;
}

/**
 * Rebuild the floating trace panel from `window.tsjs.renders`.
 *
 * Reads the whole registry each call so the panel always reflects the current
 * state; safe to call on every render event.
 */
export function renderTracePanel(): void {
  try {
    if (!traceOverlayEnabled()) return;
    const panel = ensureTracePanel();
    if (!panel) return;

    const renders = window.tsjs?.renders ?? {};
    const slots = Object.values(renders);
    // Count only slots that are honestly OK (TS creative placed and visible),
    // not merely "GAM said something rendered" — the whole point of the fix.
    const ok = slots.filter((r) => panelStatus(r) === 'ok').length;
    // Newest render first: on a page that refreshes its slots this reads as a
    // timeline rather than a set of counters.
    const history = [...(window.tsjs?.renderLog ?? [])].reverse();

    panel.replaceChildren();

    const header = document.createElement('div');
    const hs = header.style;
    hs.setProperty('display', 'flex');
    hs.setProperty('justify-content', 'space-between');
    hs.setProperty('align-items', 'center');
    hs.setProperty('gap', '8px');
    hs.setProperty('padding', '6px 10px');
    hs.setProperty('position', 'sticky');
    hs.setProperty('top', '0');
    hs.setProperty('background', '#000');
    hs.setProperty('font-weight', '700');

    const title = document.createElement('span');
    title.textContent = `TS Render Trace · ${ok}/${slots.length} slots ok · ${history.length} renders`;

    const close = document.createElement('button');
    close.textContent = '×';
    close.setAttribute('aria-label', 'Close trace panel');
    const cs = close.style;
    cs.setProperty('background', 'transparent');
    cs.setProperty('color', '#eee');
    cs.setProperty('border', '0');
    cs.setProperty('font-size', '14px');
    cs.setProperty('cursor', 'pointer');
    cs.setProperty('line-height', '1');
    close.addEventListener('click', () => panel.remove());

    header.append(title, close);
    panel.appendChild(header);

    const hint = document.createElement('div');
    hint.style.setProperty('padding', '2px 10px 4px');
    hint.style.setProperty('color', '#777');
    hint.style.setProperty('font-size', '9px');
    hint.textContent = 'newest first · click a row to copy its full record · hover for detail';
    panel.appendChild(hint);

    if (history.length === 0) {
      const empty = document.createElement('div');
      empty.style.setProperty('padding', '6px 10px');
      empty.style.setProperty('color', '#bbb');
      empty.textContent = 'No creatives traced yet.';
      panel.appendChild(empty);
      return;
    }

    for (const record of history) {
      panel.appendChild(buildPanelRow(record));
    }
  } catch (err) {
    log.warn('trace: failed to render panel', err);
  }
}

/**
 * Write a render record into `window.tsjs.renders` and fire the render event.
 *
 * Repeated records for the same slot (SPA navigation, GPT refresh) overwrite
 * the previous entry and increment `count`, so the registry always reflects
 * the latest render while preserving how many renders the slot has seen.
 * When the trace overlay is armed, the floating panel is refreshed here — the
 * single choke point every render passes through.
 */
export function recordRender(record: Omit<RenderRecord, 'count' | 'at' | 'seq'>): RenderRecord {
  const full: RenderRecord = { ...record, count: 1, seq: nextRenderSeq(), at: Date.now() };
  try {
    const ts = (window.tsjs ??= {} as LegacyTsjsApi);
    const renders = (ts.renders ??= {});
    const prev = renders[record.slotId];
    if (prev) full.count = prev.count + 1;
    renders[record.slotId] = full;
    const history = (ts.renderLog ??= []);
    history.push(full);
    if (history.length > MAX_RENDER_LOG_ENTRIES) {
      history.splice(0, history.length - MAX_RENDER_LOG_ENTRIES);
    }
  } catch (err) {
    log.warn('trace: failed to write render record', { slotId: record.slotId, err });
  }
  try {
    window.dispatchEvent(new CustomEvent(RENDER_EVENT_NAME, { detail: full }));
  } catch (err) {
    log.debug('trace: failed to dispatch render event', { slotId: record.slotId, err });
  }
  renderTracePanel();
  return full;
}

/**
 * Fields a later signal about an already-recorded render may contribute.
 * Identity (`slotId`) and bookkeeping (`seq`, `count`, `at`) are fixed at
 * [`recordRender`] time and are never revised.
 */
export type RenderUpdate = Partial<Omit<RenderRecord, 'slotId' | 'seq' | 'count' | 'at'>>;

/** Confirmation flags that a later, weaker signal must never clear. */
const CONFIRMATION_FIELDS = ['rendered', 'injected'] as const;

/**
 * Merge a later signal into an existing render record, **in place**.
 *
 * One impression can be observed twice: GAM's `slotRenderEnded` and the Prebid
 * Universal Creative bridge both describe the same GAM ad request, and a
 * deferred ADM placement resolves an animation frame after the render was first
 * recorded. Appending a second [`recordRender`] for those would inflate the
 * slot's `count`, the history length, the page-global sequence numbers and the
 * panel totals — one impression must stay one row.
 *
 * So the later signal enriches the record instead: `seq`, `count` and `at` are
 * left untouched and no new history entry is appended. Because the registry and
 * the history hold the *same* object, mutating it updates both.
 *
 * Confirmations only ever strengthen. A `false` in `patch` cannot clear a
 * `true` already on the record, so the weaker GAM-only signal arriving after
 * the bridge's confirmed placement does not erase it.
 */
export function updateRender(record: RenderRecord, patch: RenderUpdate): RenderRecord {
  try {
    const fields = record as unknown as Record<string, unknown>;
    for (const [key, value] of Object.entries(patch)) {
      if (value === undefined) continue;
      if (
        value === false &&
        fields[key] === true &&
        (CONFIRMATION_FIELDS as readonly string[]).includes(key)
      ) {
        continue;
      }
      fields[key] = value;
    }
  } catch (err) {
    log.warn('trace: failed to update render record', { slotId: record.slotId, err });
  }
  try {
    window.dispatchEvent(new CustomEvent(RENDER_EVENT_NAME, { detail: record }));
  } catch (err) {
    log.debug('trace: failed to dispatch render update event', { slotId: record.slotId, err });
  }
  renderTracePanel();
  return record;
}

/**
 * Stamp an element with `data-ts-*` attributes carrying the trace tuple, so
 * a creative in the DOM can be joined to the server-side `auction winner:` /
 * `auction delivered creative:` log lines by inspection alone.
 *
 * Attributes whose record field is absent are removed, so a re-render of the
 * same element (SPA navigation, GPT refresh) never leaves stale values from a
 * previous auction next to the new ones. These attributes live on the element
 * itself, so they survive a later `innerHTML = ''` that clears the slot's
 * children (e.g. the GAM adm interceptor) — unlike a child badge would.
 */
export function stampCreativeTrace(el: Element, record: RenderRecord): void {
  const attrs: Array<[string, string | undefined]> = [
    ['data-ts-slot-id', record.slotId],
    ['data-ts-render-path', record.path],
    ['data-ts-rendered', String(record.rendered)],
    ['data-ts-auction-id', record.auctionId],
    ['data-ts-bidder', record.bidder],
    ['data-ts-ad-id', record.adId],
    ['data-ts-bid-id', record.bidId],
    ['data-ts-creative-id', record.creativeId],
    ['data-ts-adm-hash', record.admHash],
    ['data-ts-served-from', record.servedFrom],
    ['data-ts-gam-empty', record.gamEmpty === undefined ? undefined : String(record.gamEmpty)],
    ['data-ts-injected', record.injected === undefined ? undefined : String(record.injected)],
    ['data-ts-visible', record.visible === undefined ? undefined : String(record.visible)],
  ];
  try {
    for (const [name, value] of attrs) {
      if (value !== undefined && value !== '') {
        el.setAttribute(name, value);
      } else {
        el.removeAttribute(name);
      }
    }
    // Badge any slot that actually shows something, carrying its honest status
    // colour: green ✓ for a confirmed TS render, blue ◐ for `gam-only` (GAM
    // rendered, TS cannot confirm it as its own). Slots with nothing on screen
    // (`empty`) or nothing visible (`hidden`) stay unbadged — there is no
    // creative there to label. Never badge the iframe itself.
    //
    // Any previous badge is dropped first, unconditionally, so a slot that
    // re-renders into `empty` or `hidden` sheds the badge from its last render
    // instead of contradicting the panel.
    if (el instanceof HTMLElement && el.tagName !== 'IFRAME') {
      removeTraceBadge(el);
      const status = panelStatus(record);
      if (traceOverlayEnabled() && (status === 'ok' || status === 'gam-only')) {
        attachTraceBadge(el, record);
      }
    }
  } catch (err) {
    log.warn('trace: failed to stamp element', { slotId: record.slotId, err });
  }
}

const MAX_RENDER_TRACE_SLOTS = 256;
const MAX_RENDER_TRACE_SUBSCRIBERS = 32;
const MAX_RENDER_TRACE_NOTIFICATIONS = 200;

type RenderTraceInputV1 = Omit<RenderTraceRecord, 'at' | 'count' | 'seq'>;
type RenderTraceUpdateV1 = Partial<Omit<RenderTraceRecord, 'at' | 'count' | 'seq' | 'slotId'>>;

export interface RenderTraceRuntimeScheduler {
  readonly set: (callback: () => void, milliseconds: number) => unknown;
  readonly clear: (handle: unknown) => void;
}

export interface RenderTraceRuntimeOptions {
  readonly document?: Document | undefined;
  readonly exportRecord?: (record: Readonly<RenderTraceRecord>) => void;
  readonly now?: () => number;
  readonly onOverflow?: (droppedNotifications: number) => void;
  readonly onPresentationError?: (error: unknown) => void;
  readonly onSubscriberError?: (error: unknown) => void;
  readonly overlayEnabled?: boolean;
  readonly schedule?: (callback: () => void) => () => void;
  readonly scheduler?: RenderTraceRuntimeScheduler;
}

export interface RenderTraceRuntimeOwner {
  readonly api: RenderTraceDiagnostics;
  readonly diagnostics: RenderTraceDiagnostics;
  readonly record: (input: RenderTraceInputV1) => Readonly<RenderTraceRecord>;
  readonly enrich: (
    recordOrSequence: Readonly<RenderTraceRecord> | number,
    patch: RenderTraceUpdateV1
  ) => Readonly<RenderTraceRecord> | undefined;
  readonly prune: (slotId: string, sequence?: number) => boolean;
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

function copyRenderTraceRecord(record: Readonly<RenderRecord>): Readonly<RenderTraceRecord> {
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

const RUNTIME_TRACE_ATTRIBUTES = [
  'data-ts-slot-id',
  'data-ts-render-path',
  'data-ts-rendered',
  'data-ts-auction-id',
  'data-ts-bidder',
  'data-ts-ad-id',
  'data-ts-bid-id',
  'data-ts-creative-id',
  'data-ts-adm-hash',
  'data-ts-served-from',
  'data-ts-gam-empty',
  'data-ts-injected',
  'data-ts-visible',
] as const;

interface PresentedTraceSlot {
  readonly element: HTMLElement;
  readonly priorInlinePosition?: string;
}

interface RenderTracePresentation {
  readonly present: (record: Readonly<RenderTraceRecord>) => void;
  readonly prune: (slotId: string) => void;
  readonly dispose: () => void;
}

function createRenderTracePresentation(
  options: RenderTraceRuntimeOptions,
  history: () => readonly Readonly<RenderTraceRecord>[]
): RenderTracePresentation {
  const targetDocument =
    options.document ?? (typeof document === 'undefined' ? undefined : document);
  const overlayEnabled = options.overlayEnabled === true;
  const presented = new Map<string, PresentedTraceSlot>();
  const panelRecords = new Map<number, Readonly<RenderTraceRecord>>();
  const panelRows = new Map<number, HTMLButtonElement>();
  let panel: HTMLElement | undefined;
  let panelHeading: HTMLElement | undefined;
  let panelRowsHost: HTMLElement | undefined;

  const report = (error: unknown): void => {
    try {
      options.onPresentationError?.(error);
    } catch {
      // Presentation reporting is diagnostics-only.
    }
  };

  const removeBadge = (element: HTMLElement): void => {
    for (const badge of element.querySelectorAll(`:scope > .${TRACE_BADGE_CLASS}`)) badge.remove();
  };

  const clearElement = (presentedSlot: PresentedTraceSlot): void => {
    const { element, priorInlinePosition } = presentedSlot;
    for (const attribute of RUNTIME_TRACE_ATTRIBUTES) element.removeAttribute(attribute);
    removeBadge(element);
    if (priorInlinePosition !== undefined && element.style.position === 'relative') {
      element.style.position = priorInlinePosition;
    }
  };

  const createBadge = (
    element: HTMLElement,
    record: Readonly<RenderTraceRecord>
  ): PresentedTraceSlot => {
    let priorInlinePosition: string | undefined;
    try {
      const position = targetDocument?.defaultView?.getComputedStyle(element).position;
      if (position === 'static' || position === '') {
        priorInlinePosition = element.style.position;
        element.style.position = 'relative';
      }
    } catch {
      // A badge remains noninteractive even if its containing block is publisher-owned.
    }
    const status = panelStatus(record as RenderRecord);
    const style = STATUS_STYLE[status];
    const badge = targetDocument?.createElement('div');
    if (!badge) {
      return {
        element,
        ...(priorInlinePosition === undefined ? {} : { priorInlinePosition }),
      };
    }
    badge.className = TRACE_BADGE_CLASS;
    badge.textContent =
      `TS ${style.mark} #${record.seq}` +
      `${record.bidder ? ` · ${record.bidder}` : ''}` +
      `${style.label === 'ok' ? '' : ` · ${style.label}`}`;
    badge.style.setProperty('position', 'absolute');
    badge.style.setProperty('top', '4px');
    badge.style.setProperty('left', '4px');
    badge.style.setProperty('z-index', '2147483646');
    badge.style.setProperty('pointer-events', 'none');
    badge.style.setProperty('font', '10px/1.5 ui-monospace, Menlo, Consolas, monospace');
    badge.style.setProperty('padding', '1px 5px');
    badge.style.setProperty('color', '#fff');
    badge.style.setProperty('background', style.color);
    badge.style.setProperty('border-radius', '3px');
    element.appendChild(badge);
    return { element, ...(priorInlinePosition === undefined ? {} : { priorInlinePosition }) };
  };

  const exportRow = (record: Readonly<RenderTraceRecord>): void => {
    const copied = copyRenderTraceRecord(record);
    try {
      if (options.exportRecord) {
        options.exportRecord(copied);
        return;
      }
      const clipboard = targetDocument?.defaultView?.navigator.clipboard;
      const write = clipboard?.writeText;
      if (typeof write !== 'function') return;
      const pending = Reflect.apply(write, clipboard, [JSON.stringify(copied, null, 2)]) as
        Promise<void> | undefined;
      void pending?.catch(report);
    } catch (error) {
      report(error);
    }
  };

  const renderPanel = (record?: Readonly<RenderTraceRecord>): void => {
    if (!overlayEnabled || !targetDocument?.body) return;
    if (!panel) {
      const collision = targetDocument.getElementById(TRACE_PANEL_ID);
      if (collision) return;
      panel = targetDocument.createElement('div');
      panel.id = TRACE_PANEL_ID;
      panel.setAttribute('data-ts-render-trace-owner', '1');
      panel.style.setProperty('position', 'fixed');
      panel.style.setProperty('bottom', '12px');
      panel.style.setProperty('right', '12px');
      panel.style.setProperty('z-index', '2147483647');
      panel.style.setProperty('max-width', '360px');
      panel.style.setProperty('max-height', '45vh');
      panel.style.setProperty('overflow', 'auto');
      panel.style.setProperty('background', 'rgba(17,17,17,0.94)');
      panel.style.setProperty('color', '#eee');
      panel.style.setProperty('font', '11px/1.5 ui-monospace, Menlo, Consolas, monospace');
      panel.style.setProperty('border', '1px solid #333');
      panel.style.setProperty('border-radius', '6px');
      panel.style.setProperty('box-shadow', '0 4px 16px rgba(0,0,0,0.4)');
      panelHeading = targetDocument.createElement('div');
      panelHeading.style.setProperty('padding', '6px 10px');
      panelHeading.style.setProperty('font-weight', '700');
      panelRowsHost = targetDocument.createElement('div');
      panel.append(panelHeading, panelRowsHost);
      targetDocument.body.appendChild(panel);
    }
    const retained = history();
    panelHeading!.textContent = `TS Render Trace · ${retained.length} renders`;
    const retainedSequences = new Set(retained.map(({ seq }) => seq));
    for (const [sequence, row] of panelRows) {
      if (retainedSequences.has(sequence)) continue;
      row.remove();
      panelRows.delete(sequence);
      panelRecords.delete(sequence);
    }
    if (record && retainedSequences.has(record.seq)) {
      panelRecords.set(record.seq, record);
      let row = panelRows.get(record.seq);
      if (!row) {
        row = targetDocument.createElement('button');
        row.type = 'button';
        row.setAttribute('data-ts-trace-seq', String(record.seq));
        row.style.setProperty('display', 'block');
        row.style.setProperty('width', '100%');
        row.style.setProperty('padding', '6px 10px');
        row.style.setProperty('border', '0');
        row.style.setProperty('border-top', '1px solid #2a2a2a');
        row.style.setProperty('background', 'transparent');
        row.style.setProperty('font', 'inherit');
        row.style.setProperty('text-align', 'left');
        row.style.setProperty('cursor', 'pointer');
        row.addEventListener('click', () => {
          const exported = panelRecords.get(record.seq);
          if (exported) exportRow(exported);
        });
        panelRows.set(record.seq, row);
        panelRowsHost!.prepend(row);
      }
      const status = panelStatus(record as RenderRecord);
      const style = STATUS_STYLE[status];
      row.textContent = `#${record.seq} ${style.mark} ${record.slotId} · ${style.label} · ${record.path}`;
      row.style.setProperty('border-left', `3px solid ${style.color}`);
      row.style.setProperty('color', style.color);
    }
  };

  const present = (record: Readonly<RenderTraceRecord>): void => {
    try {
      const prior = presented.get(record.slotId);
      const elementId = record.elementId ?? record.slotId;
      const candidate = targetDocument?.getElementById(elementId);
      const element = candidate && candidate instanceof HTMLElement ? candidate : undefined;
      if (prior && prior.element !== element) {
        clearElement(prior);
        presented.delete(record.slotId);
      }
      if (element) {
        const retainedPosition = prior?.element === element ? prior.priorInlinePosition : undefined;
        removeBadge(element);
        const values: Readonly<
          Record<(typeof RUNTIME_TRACE_ATTRIBUTES)[number], string | undefined>
        > = {
          'data-ts-slot-id': record.slotId,
          'data-ts-render-path': record.path,
          'data-ts-rendered': String(record.rendered),
          'data-ts-auction-id': record.auctionId,
          'data-ts-bidder': record.bidder,
          'data-ts-ad-id': record.adId,
          'data-ts-bid-id': record.bidId,
          'data-ts-creative-id': record.creativeId,
          'data-ts-adm-hash': record.admHash,
          'data-ts-served-from': record.servedFrom,
          'data-ts-gam-empty': record.gamEmpty === undefined ? undefined : String(record.gamEmpty),
          'data-ts-injected': record.injected === undefined ? undefined : String(record.injected),
          'data-ts-visible': record.visible === undefined ? undefined : String(record.visible),
        };
        for (const attribute of RUNTIME_TRACE_ATTRIBUTES) {
          const value = values[attribute];
          if (value === undefined || value === '') element.removeAttribute(attribute);
          else element.setAttribute(attribute, value);
        }
        const status = panelStatus(record as RenderRecord);
        if (
          overlayEnabled &&
          element.tagName !== 'IFRAME' &&
          (status === 'ok' || status === 'gam-only')
        ) {
          const next = createBadge(element, record);
          presented.set(record.slotId, {
            element,
            ...(retainedPosition === undefined
              ? next.priorInlinePosition === undefined
                ? {}
                : { priorInlinePosition: next.priorInlinePosition }
              : { priorInlinePosition: retainedPosition }),
          });
        } else {
          if (retainedPosition !== undefined && element.style.position === 'relative') {
            element.style.position = retainedPosition;
          }
          presented.set(record.slotId, { element });
        }
      }
      renderPanel(record);
    } catch (error) {
      report(error);
    }
  };

  const prune = (slotId: string): void => {
    try {
      const existing = presented.get(slotId);
      if (existing) clearElement(existing);
      presented.delete(slotId);
      renderPanel();
    } catch (error) {
      report(error);
    }
  };

  const dispose = (): void => {
    for (const slotId of [...presented.keys()]) prune(slotId);
    try {
      panel?.remove();
    } catch (error) {
      report(error);
    }
    panel = undefined;
    panelHeading = undefined;
    panelRowsHost = undefined;
    panelRecords.clear();
    panelRows.clear();
  };

  return Object.freeze({ present, prune, dispose });
}

/** Create one document-runtime render trace without exposing its mutation authority. */
export function createRenderTraceDiagnostics(
  options: RenderTraceRuntimeOptions = {}
): RenderTraceRuntimeOwner {
  const current = new Map<string, Readonly<RenderTraceRecord>>();
  const history: Array<Readonly<RenderTraceRecord>> = [];
  const recordsBySequence = new Map<number, Readonly<RenderTraceRecord>>();
  const subscribers = new Map<number, RenderTraceSubscription>();
  const pendingOrder: number[] = [];
  const pendingBySequence = new Map<number, PendingRenderTraceNotification>();
  let sequence = 0;
  let subscriberSequence = 0;
  let droppedNotifications = 0;
  let reportedDroppedNotifications = 0;
  let cancelScheduled: (() => void) | undefined;
  let disposed = false;
  const presentation = createRenderTracePresentation(options, () => history);

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

  const record = (input: RenderTraceInputV1): Readonly<RenderTraceRecord> => {
    const previous = current.get(input.slotId);
    let at: number;
    try {
      at = (options.now ?? Date.now)();
    } catch {
      at = Date.now();
    }
    const committed = copyRenderTraceRecord({
      ...input,
      count: (previous?.count ?? 0) + 1,
      seq: (sequence += 1),
      at,
    });
    if (disposed) return committed;
    if (!previous && current.size >= MAX_RENDER_TRACE_SLOTS) {
      const oldestSlot = current.keys().next().value as string | undefined;
      if (oldestSlot !== undefined) {
        current.delete(oldestSlot);
        presentation.prune(oldestSlot);
      }
    }
    current.set(committed.slotId, committed);
    recordsBySequence.set(committed.seq, committed);
    history.push(committed);
    if (history.length > MAX_RENDER_LOG_ENTRIES) {
      const evicted = history.shift();
      if (evicted && !retained(evicted)) recordsBySequence.delete(evicted.seq);
    }
    if (previous && !retained(previous)) recordsBySequence.delete(previous.seq);
    enqueue(committed);
    presentation.present(committed);
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
    presentation.present(committed);
    return committed;
  };

  const prune = (slotId: string, expectedSequence?: number): boolean => {
    if (disposed || typeof slotId !== 'string') return false;
    const existing = current.get(slotId);
    if (!existing || (expectedSequence !== undefined && existing.seq !== expectedSequence)) {
      return false;
    }
    current.delete(slotId);
    if (!retained(existing)) recordsBySequence.delete(existing.seq);
    presentation.prune(slotId);
    return true;
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

  const dispose = (): void => {
    if (disposed) return;
    disposed = true;
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
    presentation.dispose();
  };

  return Object.freeze({ api, diagnostics: api, record, enrich, prune, dispose });
}

/** Short name used by the browser composition owner. */
export const createRenderTrace = createRenderTraceDiagnostics;
