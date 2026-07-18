import { describe, it, expect, vi, beforeEach } from 'vitest';
import { recordRender, stampCreativeTrace, RENDER_EVENT_NAME } from '../../src/core/trace';
import type { RenderRecord, TsjsApi } from '../../src/core/types';

describe('trace/recordRender', () => {
  beforeEach(() => {
    delete (window as { tsjs?: TsjsApi }).tsjs;
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
