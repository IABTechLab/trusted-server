import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';

declare global {
  interface Window {
    tsjs?: any;
  }
}

const ORIGINAL_FETCH = global.fetch;

describe('core/index', () => {
  beforeEach(async () => {
    await vi.resetModules();
    document.body.innerHTML = '';
    delete (window as any).tsjs;
  });

  afterEach(() => {
    global.fetch = ORIGINAL_FETCH;
  });

  it('initializes tsjs API with expected surface', async () => {
    await import('../../src/core/index');
    const api = window.tsjs;
    expect(api).toBeDefined();
    expect(typeof api.version).toBe('string');
    expect(Array.isArray(api.que)).toBe(true);
    expect(typeof api.addAdUnits).toBe('function');
    expect(typeof api.renderAdUnit).toBe('function');
    expect(typeof api.renderAllAdUnits).toBe('function');
    expect(typeof api.setConfig).toBe('function');
    expect(typeof api.getConfig).toBe('function');
    expect(typeof api.requestAds).toBe('function');
  });

  it('flushes queued callbacks that existed before initialization', async () => {
    const callback = vi.fn(function () {
      expect(this).toBe(window.tsjs);
    });
    (window as any).tsjs = { que: [callback] };

    await import('../../src/core/index');

    expect(callback).toHaveBeenCalledTimes(1);
  });

  it('installs queue that executes callbacks immediately with api context', async () => {
    await import('../../src/core/index');
    const api = window.tsjs;
    const fn = vi.fn();

    api.que.push(fn);

    expect(fn).toHaveBeenCalledTimes(1);
    expect(fn.mock.instances[0]).toBe(api);
  });

  it('renders registered ad units using core rendering helpers', async () => {
    await import('../../src/core/index');
    const api = window.tsjs;

    api.addAdUnits([
      { code: 'slot-1', mediaTypes: { banner: { sizes: [[300, 250]] } } },
      { code: 'slot-2', mediaTypes: { banner: { sizes: [[320, 50]] } } },
    ]);

    api.renderAllAdUnits();

    expect(document.getElementById('slot-1')?.textContent).toContain('300x250');
    expect(document.getElementById('slot-2')?.textContent).toContain('320x50');
  });

  it('exposes requestAds from the core request module', async () => {
    const { requestAds } = await import('../../src/core/request');
    await import('../../src/core/index');
    const api = window.tsjs;

    expect(api.requestAds).toBe(requestAds);
  });
});
