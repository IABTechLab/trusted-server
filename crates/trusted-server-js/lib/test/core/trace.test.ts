import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import {
  recordRender,
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
      at: 1,
    };
    stampCreativeTrace(el, first);

    const second: RenderRecord = {
      slotId: 'slot-1',
      path: 'ssat',
      rendered: true,
      auctionId: 'auction-new',
      count: 2,
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
  const record: Omit<RenderRecord, 'count' | 'at'> = {
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
    expect(panels[0].textContent).toContain('#2');
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
      renders: { 'slot-1': { ...record, count: 1, at: 1 } },
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
    at: 1,
  };

  it('badges an ok slot when armed', () => {
    document.cookie = 'ts-trace=1; Path=/';
    const el = document.createElement('div');
    document.body.appendChild(el);
    stampCreativeTrace(el, okRecord);
    const badge = el.querySelector(`.${TRACE_BADGE_CLASS}`) as HTMLElement;
    expect(badge).toBeTruthy();
    expect(badge.textContent).toBe('TS ✓ mocktioneer');
  });

  it('does not badge a hidden slot', () => {
    document.cookie = 'ts-trace=1; Path=/';
    const el = document.createElement('div');
    document.body.appendChild(el);
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
