// Render-trace registry and DOM markers: joins a creative rendered on the
// page back to the winning server-side auction bid. Every render writes a
// RenderRecord to window.tsjs.renders (keyed by slot ID), stamps the slot
// element with data-ts-* attributes carrying the same trace tuple, and fires
// a 'tsjs:adRendered' CustomEvent so tests and tooling can await renders.
import { log } from './log';
import type { RenderRecord, TsjsApi } from './types';

/** CustomEvent fired on window after each render-trace record is written. */
export const RENDER_EVENT_NAME = 'tsjs:adRendered';

/**
 * Write a render record into `window.tsjs.renders` and fire the render event.
 *
 * Repeated records for the same slot (SPA navigation, GPT refresh) overwrite
 * the previous entry and increment `count`, so the registry always reflects
 * the latest render while preserving how many renders the slot has seen.
 */
export function recordRender(record: Omit<RenderRecord, 'count' | 'at'>): RenderRecord {
  const full: RenderRecord = { ...record, count: 1, at: Date.now() };
  try {
    const ts = (window.tsjs ??= {} as TsjsApi);
    const renders = (ts.renders ??= {});
    const prev = renders[record.slotId];
    if (prev) full.count = prev.count + 1;
    renders[record.slotId] = full;
  } catch (err) {
    log.warn('trace: failed to write render record', { slotId: record.slotId, err });
  }
  try {
    window.dispatchEvent(new CustomEvent(RENDER_EVENT_NAME, { detail: full }));
  } catch (err) {
    // CustomEvent unavailable — registry entry above is still written.
    log.debug('trace: failed to dispatch render event', { slotId: record.slotId, err });
  }
  return full;
}

/**
 * Stamp an element with `data-ts-*` attributes carrying the trace tuple, so
 * a creative in the DOM can be joined to the server-side `auction winner:` /
 * `auction delivered creative:` log lines by inspection alone.
 *
 * Attributes whose record field is absent are removed, so a re-render of the
 * same element (SPA navigation, GPT refresh) never leaves stale values from a
 * previous auction next to the new ones.
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
  ];
  try {
    for (const [name, value] of attrs) {
      if (value !== undefined && value !== '') {
        el.setAttribute(name, value);
      } else {
        el.removeAttribute(name);
      }
    }
  } catch (err) {
    log.warn('trace: failed to stamp element', { slotId: record.slotId, err });
  }
}
