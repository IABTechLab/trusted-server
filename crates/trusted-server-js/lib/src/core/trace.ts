// Render-trace registry, DOM markers, and a floating debug panel: joins a
// creative rendered on the page back to the winning server-side auction bid.
// Every render writes a RenderRecord to window.tsjs.renders (keyed by slot ID),
// stamps the slot element with data-ts-* attributes carrying the same trace
// tuple, and fires a 'tsjs:adRendered' CustomEvent. When the ts-trace cookie is
// armed (via GET /_ts/trace), a Google-Publisher-Console-style overlay panel
// summarises every traced slot so an operator can confirm on the page itself
// that creatives came through Trusted Server — on both the SSAT/GAM and
// /auction render paths.
import { log } from './log';
import type { RenderRecord, TsjsApi } from './types';

/** CustomEvent fired on window after each render-trace record is written. */
export const RENDER_EVENT_NAME = 'tsjs:adRendered';

/**
 * Cookie armed by `GET /_ts/trace` (server-side, `ts-trace=1`). While present,
 * the floating trace panel is shown so an operator can see on the page itself
 * that creatives were delivered by Trusted Server.
 */
const TRACE_COOKIE_NAME = 'ts-trace';

/** DOM id of the floating trace panel (body-level overlay). */
export const TRACE_PANEL_ID = 'ts-render-trace-panel';

/**
 * Upper bound on `window.tsjs.renderLog`. A publisher page that refreshes its
 * slots on every render can produce hundreds of entries in a session, so the
 * history is trimmed from the front rather than growing without limit.
 */
const MAX_RENDER_LOG_ENTRIES = 200;

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
  if (record.visible === false) return 'hidden';
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
  el.querySelectorAll(`:scope > .${TRACE_BADGE_CLASS}`).forEach((n) => n.remove());

  const position = getComputedStyle(el).position;
  if (position === 'static' || position === '') {
    el.style.position = 'relative';
  }

  const badge = document.createElement('div');
  badge.className = TRACE_BADGE_CLASS;
  badge.textContent = `TS ${style.mark} ${record.bidder ?? '?'}${style.label === 'ok' ? '' : ` · ${style.label}`}`;
  badge.title = [
    `slot: ${record.slotId}`,
    `auction: ${record.auctionId ?? '?'}`,
    `bidder: ${record.bidder ?? '?'}`,
    `creative: ${record.creativeId ?? '?'}`,
    `adm_hash: ${record.admHash ?? '?'}`,
    `served: ${record.servedFrom ?? '?'}`,
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

/** GAM/injection state summary for the panel's detail line. */
function stateSummary(record: RenderRecord): string {
  const parts: string[] = [];
  if (record.path === 'ssat') {
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
    `slot: ${record.slotId}`,
    `status: ${style.label}`,
    `path: ${record.path}`,
    `rendered (gam non-empty): ${record.rendered}`,
    `gam_empty: ${record.gamEmpty ?? '—'}`,
    `injected (ts placed): ${record.injected ?? '—'}`,
    `visible: ${record.visible ?? '—'}`,
    `auction: ${record.auctionId ?? '?'}`,
    `bidder: ${record.bidder ?? '?'}`,
    `creative: ${record.creativeId ?? '?'}`,
    `ad_id: ${record.adId ?? '?'}`,
    `adm_hash: ${record.admHash ?? '?'}`,
    `served: ${record.servedFrom ?? '?'}`,
    `element: ${record.elementId ?? '?'}`,
    `renders: ${record.count}`,
  ].join('\n');

  const line1 = document.createElement('div');
  const clock = new Date(record.at).toLocaleTimeString('en-GB', { hour12: false });
  line1.textContent = `${clock} ${style.mark} ${record.slotId} · ${style.label}`;
  line1.style.setProperty('font-weight', '600');
  line1.style.setProperty('color', style.color);

  const line2 = document.createElement('div');
  line2.style.setProperty('color', '#bbb');
  line2.textContent = `${record.path}${mechanismSuffix(record)} · ${record.bidder ?? '?'} · ${short(record.admHash)}`;

  const line3 = document.createElement('div');
  line3.style.setProperty('color', '#777');
  line3.textContent = `${stateSummary(record)} · auction ${short(record.auctionId)} · #${record.count}`;

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
export function recordRender(record: Omit<RenderRecord, 'count' | 'at'>): RenderRecord {
  const full: RenderRecord = { ...record, count: 1, at: Date.now() };
  try {
    const ts = (window.tsjs ??= {} as TsjsApi);
    const renders = (ts.renders ??= {});
    const prev = renders[record.slotId];
    if (prev) full.count = prev.count + 1;
    renders[record.slotId] = full;

    // Keep each render as its own history entry, trimmed from the front.
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
    // CustomEvent unavailable — registry entry above is still written.
    log.debug('trace: failed to dispatch render event', { slotId: record.slotId, err });
  }
  renderTracePanel();
  return full;
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
    const status = panelStatus(record);
    if (
      el instanceof HTMLElement &&
      el.tagName !== 'IFRAME' &&
      traceOverlayEnabled() &&
      (status === 'ok' || status === 'gam-only')
    ) {
      attachTraceBadge(el, record);
    }
  } catch (err) {
    log.warn('trace: failed to stamp element', { slotId: record.slotId, err });
  }
}
