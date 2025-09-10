import { describe, it, expect, beforeEach, vi } from 'vitest';
import '../src/index';

declare global {
  interface Window {
    tsjs: any;
  }
}

function cleanupDom() {
  document.body.innerHTML = '';
}

describe('tsjs', () => {
  beforeEach(() => {
    cleanupDom();
  });

  it('exposes version and queue', () => {
    expect(window.tsjs).toBeDefined();
    expect(typeof window.tsjs.version).toBe('string');
    expect(Array.isArray(window.tsjs.que)).toBe(true);
  });

  it('adds ad units and renders one', () => {
    window.tsjs.addAdUnits({ code: 'slot-1', mediaTypes: { banner: { sizes: [[300, 250]] } } });
    window.tsjs.renderAdUnit('slot-1');
    const el = document.getElementById('slot-1')!;
    expect(el).toBeTruthy();
    expect(el.textContent).toContain('Trusted Server — 300x250');
  });

  it('renders all ad units', () => {
    window.tsjs.addAdUnits([
      { code: 'a', mediaTypes: { banner: { sizes: [[320, 50]] } } },
      { code: 'b', mediaTypes: { banner: { sizes: [[728, 90]] } } },
    ]);
    window.tsjs.renderAllAdUnits();
    expect(document.getElementById('a')!.textContent).toContain('320x50');
    expect(document.getElementById('b')!.textContent).toContain('728x90');
  });

  it('aliases pbjs to the same object and flushes pbjs.que', async () => {
    cleanupDom();
    (window as any).pbjs = { que: [] };
    (window as any).pbjs.que.push(function () {
      window.pbjs.setConfig({ debug: true, mode: 'thirdParty' });
      window.pbjs.addAdUnits({ code: 'pbslot', mediaTypes: { banner: { sizes: [[300, 250]] } } });
      window.pbjs.requestBids({ bidsBackHandler: () => {} });
    });
    vi.resetModules();
    await import('../src/index');
    await import('../src/ext/ext.entry');

    expect(window.tsjs).toBe(window.pbjs);
    const el = document.getElementById('pbslot');
    expect(el).toBeTruthy();
    expect(el!.textContent).toContain('300x250');
  });

  it('requestBids invokes callback and renders', () => {
    // Ensure prebid extension is installed
    // eslint-disable-next-line @typescript-eslint/no-floating-promises
    import('../src/ext/ext.entry');
    let called = false;
    window.tsjs.setConfig({ mode: 'thirdParty' } as any);
    window.tsjs.addAdUnits({ code: 'rb', mediaTypes: { banner: { sizes: [[320, 50]] } } });
    window.pbjs.requestBids({
      bidsBackHandler: () => {
        called = true;
      },
    });
    expect(called).toBe(true);
    expect(document.getElementById('rb')!.textContent).toContain('320x50');
  });

  it('flushes pre-init queue', async () => {
    cleanupDom();
    (window as any).tsjs = { que: [] };
    (window as any).tsjs.que.push(function () {
      window.tsjs.addAdUnits({ code: 'qslot', mediaTypes: { banner: { sizes: [[300, 250]] } } });
      window.tsjs.renderAllAdUnits();
    });
    vi.resetModules();
    await import('../src/index');
    const el = document.getElementById('qslot');
    expect(el).toBeTruthy();
    expect(el!.textContent).toContain('300x250');
  });
});
