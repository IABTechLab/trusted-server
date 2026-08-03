import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { execFileSync } from 'node:child_process';
import {
  recordRender,
  updateRender,
  stampCreativeTrace,
  traceOverlayEnabled,
  renderTracePanel,
  RENDER_EVENT_NAME,
  TRACE_PANEL_ID,
  TRACE_BADGE_CLASS,
} from '../../src/core/trace';
import type { RenderRecord, TsjsApi } from '../../src/core/types';

function clearTraceCookie(): void {
  document.cookie = 'ts-trace=; Max-Age=0; Path=/';
}

function removePanel(): void {
  document.getElementById(TRACE_PANEL_ID)?.remove();
}

describe('trace/recordRender', () => {
  beforeEach(() => {
    delete (window as { tsjs?: TsjsApi }).tsjs;
    clearTraceCookie();
    removePanel();
  });

  it('writes a render record into window.tsjs.renders', () => {
    const record = recordRender({
      slotId: 'slot-1',
      path: 'auction',
      rendered: true,
      elementId: 'slot-1',
      auctionId: 'auction-abc',
      bidder: 'kargo',
      creativeId: 'cr-1',
      admHash: 'a1b2c3d4e5f60718',
      servedFrom: 'inline',
    });

    expect(window.tsjs?.renders?.['slot-1']).toEqual(record);
    expect(record.count).toBe(1);
    expect(record.at).toBeGreaterThan(0);
  });

  it('overwrites the previous record and increments count on re-render', () => {
    recordRender({ slotId: 'slot-1', path: 'ssat', rendered: true, auctionId: 'a-1' });
    const second = recordRender({
      slotId: 'slot-1',
      path: 'ssat',
      rendered: true,
      auctionId: 'a-2',
    });

    const entry = window.tsjs?.renders?.['slot-1'];
    expect(entry?.auctionId).toBe('a-2');
    expect(entry?.count).toBe(2);
    expect(second.count).toBe(2);
  });

  it('fires a tsjs:adRendered CustomEvent with the record as detail', () => {
    const listener = vi.fn();
    window.addEventListener(RENDER_EVENT_NAME, listener);

    const record = recordRender({ slotId: 'slot-ev', path: 'auction', rendered: true });

    expect(listener).toHaveBeenCalledTimes(1);
    const event = listener.mock.calls[0][0] as CustomEvent<RenderRecord>;
    expect(event.detail).toEqual(record);

    window.removeEventListener(RENDER_EVENT_NAME, listener);
  });

  it('allocates one sequence across separately bundled IIFEs', async () => {
    const buildTraceBundle = (): string =>
      execFileSync(
        './node_modules/.bin/esbuild',
        ['--bundle', '--format=iife', '--platform=browser', '--loader=ts'],
        {
          cwd: process.cwd(),
          encoding: 'utf8',
          input:
            'import { recordRender } from "./src/core/trace.ts";' +
            'window.__recordFromTraceBundle = recordRender;',
        }
      );

    const firstBundle = buildTraceBundle();
    const secondBundle = buildTraceBundle();
    const testWindow = window as typeof window & {
      __recordFromTraceBundle?: typeof recordRender;
    };

    Function(firstBundle)();
    const firstRecord = testWindow.__recordFromTraceBundle!;
    const first = firstRecord({ slotId: 'iife-a', path: 'auction', rendered: true });

    Function(secondBundle)();
    const secondRecord = testWindow.__recordFromTraceBundle!;
    const second = secondRecord({ slotId: 'iife-b', path: 'ssat', rendered: true });

    expect(second.seq).toBe(first.seq + 1);
    expect(window.tsjs?.renderSeq).toBe(second.seq);
    delete testWindow.__recordFromTraceBundle;
  });

  it('enriches an existing impression without changing its bookkeeping', () => {
    const original = recordRender({
      slotId: 'slot-enrich',
      path: 'ssat',
      rendered: true,
      injected: false,
      servedFrom: 'gam',
    });
    const bookkeeping = {
      seq: original.seq,
      count: original.count,
      at: original.at,
      historyLength: window.tsjs?.renderLog?.length,
    };

    const updated = updateRender(original, { injected: true, servedFrom: 'pbs-cache' });

    expect(updated).toBe(original);
    expect(window.tsjs?.renders?.['slot-enrich']).toBe(original);
    expect(window.tsjs?.renderLog?.[0]).toBe(original);
    expect(updated).toEqual(expect.objectContaining({ injected: true, servedFrom: 'pbs-cache' }));
    expect({
      seq: updated.seq,
      count: updated.count,
      at: updated.at,
      historyLength: window.tsjs?.renderLog?.length,
    }).toEqual(bookkeeping);
  });
});

describe('trace/stampCreativeTrace', () => {
  it('stamps data-ts-* attributes for present fields only', () => {
    const el = document.createElement('div');
    const record: RenderRecord = {
      slotId: 'slot-1',
      path: 'ssat',
      rendered: true,
      auctionId: 'ts-req-abc',
      bidder: 'kargo',
      adId: 'cache-uuid-1',
      admHash: 'a1b2c3d4e5f60718',
      count: 1,
      seq: 1,
      at: 1,
    };

    stampCreativeTrace(el, record);

    expect(el.getAttribute('data-ts-slot-id')).toBe('slot-1');
    expect(el.getAttribute('data-ts-render-path')).toBe('ssat');
    expect(el.getAttribute('data-ts-rendered')).toBe('true');
    expect(el.getAttribute('data-ts-auction-id')).toBe('ts-req-abc');
    expect(el.getAttribute('data-ts-bidder')).toBe('kargo');
    expect(el.getAttribute('data-ts-ad-id')).toBe('cache-uuid-1');
    expect(el.getAttribute('data-ts-adm-hash')).toBe('a1b2c3d4e5f60718');
    // creativeId absent — attribute must not exist.
    expect(el.hasAttribute('data-ts-creative-id')).toBe(false);
  });

  it('removes stale attributes when a re-render lacks a field', () => {
    const el = document.createElement('div');
    const first: RenderRecord = {
      slotId: 'slot-1',
      path: 'ssat',
      rendered: true,
      auctionId: 'auction-old',
      admHash: 'a1b2c3d4e5f60718',
      servedFrom: 'gam',
      count: 1,
      seq: 1,
      at: 1,
    };
    stampCreativeTrace(el, first);

    const second: RenderRecord = {
      slotId: 'slot-1',
      path: 'ssat',
      rendered: true,
      auctionId: 'auction-new',
      count: 2,
      seq: 2,
      at: 2,
    };
    stampCreativeTrace(el, second);

    expect(el.getAttribute('data-ts-auction-id')).toBe('auction-new');
    // The previous auction's hash and mechanism must not survive the re-stamp.
    expect(el.hasAttribute('data-ts-adm-hash')).toBe(false);
    expect(el.hasAttribute('data-ts-served-from')).toBe(false);
  });
});

describe('trace/floating panel', () => {
  const record: Omit<RenderRecord, 'count' | 'at' | 'seq'> = {
    slotId: 'slot-1',
    path: 'ssat',
    rendered: true,
    injected: true,
    visible: true,
    gamEmpty: false,
    auctionId: 'ts-req-abcdef123456',
    bidder: 'kargo',
    admHash: 'a1b2c3d4e5f60718',
    servedFrom: 'gam',
  };

  beforeEach(() => {
    delete (window as { tsjs?: TsjsApi }).tsjs;
    clearTraceCookie();
    removePanel();
  });

  afterEach(() => {
    clearTraceCookie();
    removePanel();
  });

  it('reports the overlay disabled without the ts-trace cookie', () => {
    expect(traceOverlayEnabled()).toBe(false);
  });

  it('does not create a panel when the overlay is disarmed', () => {
    recordRender(record);
    expect(document.getElementById(TRACE_PANEL_ID)).toBeNull();
  });

  it('renders a panel row per traced slot with honest status', () => {
    document.cookie = 'ts-trace=1; Path=/';
    // slot-1: TS placed + visible → ok. slot-2: nothing rendered → empty.
    recordRender(record);
    recordRender({
      slotId: 'slot-2',
      path: 'auction',
      rendered: false,
      injected: false,
      visible: false,
      bidder: 'appnexus',
    });

    const panel = document.getElementById(TRACE_PANEL_ID);
    expect(panel).toBeTruthy();
    // Only slot-1 is honestly ok; slot-2 rendered nothing.
    expect(panel!.textContent).toContain('TS Render Trace · 1/2 slots ok');
    expect(panel!.textContent).toContain('✓ slot-1 · ok');
    expect(panel!.textContent).toContain('✗ slot-2 · empty');
    expect(panel!.textContent).toContain('ssat · kargo');
    expect(panel!.textContent).toContain('auction · appnexus');
  });

  it('marks a rendered-but-hidden slot as hidden, not ok', () => {
    document.cookie = 'ts-trace=1; Path=/';
    // GAM rendered non-empty, TS injected, but a reveal gate keeps it hidden.
    recordRender({ ...record, visible: false });

    const panel = document.getElementById(TRACE_PANEL_ID);
    expect(panel!.textContent).toContain('0/1 slots ok');
    expect(panel!.textContent).toContain('⚠ slot-1 · hidden');
  });

  it('marks a targeting-only GAM slot as gam-only, not a confirmed TS render', () => {
    document.cookie = 'ts-trace=1; Path=/';
    // GAM rendered something, but TS never placed it (prod targeting path).
    recordRender({ ...record, injected: false, gamEmpty: false, visible: true });

    const panel = document.getElementById(TRACE_PANEL_ID);
    expect(panel!.textContent).toContain('0/1 slots ok');
    expect(panel!.textContent).toContain('◐ slot-1 · gam-only');
  });

  it('never claims ok when a render path did not report placement', () => {
    document.cookie = 'ts-trace=1; Path=/';
    // Regression: an unset `injected` must not fall through to ok — that would
    // claim a confirmed TS render for a slot TS only targeted.
    const { injected: _omitted, ...withoutInjected } = record;
    recordRender({ ...withoutInjected, gamEmpty: false, visible: true });

    const panel = document.getElementById(TRACE_PANEL_ID);
    expect(panel!.textContent).toContain('0/1 slots ok');
    expect(panel!.textContent).toContain('◐ slot-1 · gam-only');
  });

  it('reuses a single panel across renders and reflects the latest count', () => {
    document.cookie = 'ts-trace=1; Path=/';
    recordRender(record);
    recordRender(record);

    const panels = document.querySelectorAll(`#${TRACE_PANEL_ID}`);
    expect(panels).toHaveLength(1);
    // Second render of the same slot bumps the count and appends a history row.
    expect(panels[0].textContent).toContain('TS Render Trace · 1/1 slots ok');
    expect(panels[0].textContent).toContain('×2');
  });

  it("keeps GAM's fill signal and drops ? placeholders on an unattributed refresh", () => {
    document.cookie = 'ts-trace=1; Path=/';
    // A publisher-driven GAM refresh: TS ran no auction for it, so there is no
    // bidder/hash/auction id — but GAM still reported whether it filled, and
    // that is the most useful field on the row.
    recordRender({
      slotId: 'slot-1',
      path: 'gam-refresh',
      rendered: true,
      gamEmpty: false,
      injected: false,
      visible: true,
      servedFrom: 'gam',
    });

    const panel = document.getElementById(TRACE_PANEL_ID)!;
    expect(panel.textContent).toContain('gam:filled');
    expect(panel.textContent).toContain('no TS attribution');
    // Absent attribution must not render as a failed lookup, and an auction
    // segment must not appear at all when there is no auction to name.
    expect(panel.textContent).not.toContain('· ? ·');
    expect(panel.textContent).not.toContain('auction ?');
  });

  it('still reports gam:empty for a refresh GAM declined to fill', () => {
    document.cookie = 'ts-trace=1; Path=/';
    recordRender({
      slotId: 'slot-1',
      path: 'gam-refresh',
      rendered: false,
      gamEmpty: true,
      injected: false,
      visible: true,
    });

    const panel = document.getElementById(TRACE_PANEL_ID)!;
    expect(panel.textContent).toContain('gam:empty');
    expect(panel.textContent).toContain('✗ slot-1 · empty');
  });

  it('gives each render a page-global seq the badge and its panel row share', () => {
    document.cookie = 'ts-trace=1; Path=/';
    const el = document.createElement('div');
    el.id = 'slot-el';
    document.body.appendChild(el);

    // Two slots interleaved: seq must be unique page-wide, not per-slot, so a
    // badge reading #N identifies exactly one row.
    const first = recordRender(record);
    const other = recordRender({ ...record, slotId: 'slot-2' });
    expect(other.seq).toBe(first.seq + 1);

    stampCreativeTrace(el, other);
    const badge = el.querySelector(`.${TRACE_BADGE_CLASS}`) as HTMLElement;
    expect(badge.textContent).toContain(`#${other.seq}`);
    // The same number appears on that render's row in the panel.
    expect(document.getElementById(TRACE_PANEL_ID)!.textContent).toContain(`#${other.seq}`);

    el.remove();
  });

  it('marks only the live render for a slot as current', () => {
    document.cookie = 'ts-trace=1; Path=/';
    recordRender(record);
    const latest = recordRender(record);

    const panel = document.getElementById(TRACE_PANEL_ID)!;
    // Both renders are in the log, but only the newest is still on screen.
    expect(panel.textContent).toContain(`#${latest.seq}`);
    expect(panel.textContent!.match(/◂ current/g)).toHaveLength(1);
  });

  it('uses record identity when duplicate sequence values exist', () => {
    document.cookie = 'ts-trace=1; Path=/';
    const oldRecord = { ...record, auctionId: 'auction-old', count: 1, seq: 7, at: 1 };
    const liveRecord = { ...record, auctionId: 'auction-live', count: 2, seq: 7, at: 2 };
    (window as { tsjs?: TsjsApi }).tsjs = {
      renders: { 'slot-1': liveRecord },
      renderLog: [oldRecord, liveRecord],
    } as unknown as TsjsApi;

    renderTracePanel();

    const rows = [...document.querySelectorAll(`#${TRACE_PANEL_ID} div[style*="cursor"]`)];
    const oldRow = rows.find((row) => row.getAttribute('title')?.includes('auction: auction-old'));
    const liveRow = rows.find((row) =>
      row.getAttribute('title')?.includes('auction: auction-live')
    );
    expect(oldRow?.textContent).not.toContain('◂ current');
    expect(liveRow?.textContent).toContain('◂ current');
  });

  it('close button removes the panel', () => {
    document.cookie = 'ts-trace=1; Path=/';
    recordRender(record);
    const panel = document.getElementById(TRACE_PANEL_ID)!;
    const close = panel.querySelector('button') as HTMLButtonElement;
    close.click();
    expect(document.getElementById(TRACE_PANEL_ID)).toBeNull();
  });

  it('renderTracePanel is a no-op while disarmed even if renders exist', () => {
    (window as { tsjs?: TsjsApi }).tsjs = {
      renders: { 'slot-1': { ...record, count: 1, seq: 1, at: 1 } },
    } as unknown as TsjsApi;
    renderTracePanel();
    expect(document.getElementById(TRACE_PANEL_ID)).toBeNull();
  });

  it('clicking a row logs the full record', async () => {
    document.cookie = 'ts-trace=1; Path=/';
    const { log } = await import('../../src/core/log');
    const infoSpy = vi.spyOn(log, 'info').mockImplementation(() => undefined);

    recordRender(record);
    const row = document.getElementById(TRACE_PANEL_ID)!.querySelector('div[style*="cursor"]');
    (row as HTMLElement).click();

    const call = infoSpy.mock.calls.find(([m]) => m === 'trace: render record');
    expect(call?.[1]).toEqual(
      expect.objectContaining({ slotId: 'slot-1', auctionId: record.auctionId })
    );
    infoSpy.mockRestore();
  });
});

describe('trace/confirmation badge', () => {
  beforeEach(() => {
    delete (window as { tsjs?: TsjsApi }).tsjs;
    clearTraceCookie();
    document.body.innerHTML = '';
  });
  afterEach(() => {
    clearTraceCookie();
    document.body.innerHTML = '';
  });

  const okRecord: RenderRecord = {
    slotId: 'slot-1',
    path: 'ssat',
    rendered: true,
    injected: true,
    visible: true,
    gamEmpty: false,
    bidder: 'mocktioneer',
    admHash: 'a1b2c3d4e5f60718',
    servedFrom: 'gam',
    count: 1,
    seq: 1,
    at: 1,
  };

  it('badges an ok slot when armed', () => {
    document.cookie = 'ts-trace=1; Path=/';
    const el = document.createElement('div');
    document.body.appendChild(el);
    stampCreativeTrace(el, okRecord);
    const badge = el.querySelector(`.${TRACE_BADGE_CLASS}`) as HTMLElement;
    expect(badge).toBeTruthy();
    expect(badge.textContent).toBe('TS ✓ #1 · mocktioneer');
  });

  it('does not badge a hidden slot', () => {
    document.cookie = 'ts-trace=1; Path=/';
    const el = document.createElement('div');
    document.body.appendChild(el);
    stampCreativeTrace(el, { ...okRecord, visible: false });
    expect(el.querySelector(`.${TRACE_BADGE_CLASS}`)).toBeNull();
  });

  it('removes the previous badge when a filled slot becomes empty', () => {
    document.cookie = 'ts-trace=1; Path=/';
    const el = document.createElement('div');
    document.body.appendChild(el);
    stampCreativeTrace(el, okRecord);
    expect(el.querySelector(`.${TRACE_BADGE_CLASS}`)).toBeTruthy();

    stampCreativeTrace(el, { ...okRecord, rendered: false, injected: false, gamEmpty: true });

    expect(el.querySelector(`.${TRACE_BADGE_CLASS}`)).toBeNull();
  });

  it('removes the previous badge when a visible slot becomes hidden', () => {
    document.cookie = 'ts-trace=1; Path=/';
    const el = document.createElement('div');
    document.body.appendChild(el);
    stampCreativeTrace(el, okRecord);
    expect(el.querySelector(`.${TRACE_BADGE_CLASS}`)).toBeTruthy();

    stampCreativeTrace(el, { ...okRecord, visible: false });

    expect(el.querySelector(`.${TRACE_BADGE_CLASS}`)).toBeNull();
  });

  it('does not badge when the overlay is disarmed', () => {
    clearTraceCookie();
    const el = document.createElement('div');
    document.body.appendChild(el);
    stampCreativeTrace(el, okRecord);
    expect(el.querySelector(`.${TRACE_BADGE_CLASS}`)).toBeNull();
  });

  it('never badges an iframe element', () => {
    document.cookie = 'ts-trace=1; Path=/';
    const iframe = document.createElement('iframe');
    document.body.appendChild(iframe);
    stampCreativeTrace(iframe, okRecord);
    expect(iframe.querySelector(`.${TRACE_BADGE_CLASS}`)).toBeNull();
    // Attributes still stamped on the iframe though.
    expect(iframe.getAttribute('data-ts-slot-id')).toBe('slot-1');
  });
});
