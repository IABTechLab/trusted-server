import { readFileSync } from 'node:fs';
import path from 'node:path';

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

/**
 * Executable coverage for the edge-injected `gpt_bootstrap.js` — the
 * head-inline fallback that keeps initial server-side ads working when the
 * main TSJS bundle fails to load. The file ships from
 * `crates/trusted-server-core/src/integrations/gpt_bootstrap.js` and is
 * evaluated here verbatim, so the degradation path (fallback `adInit` and
 * fallback `scheduleInitialAdInit`) is executed, not string-matched.
 *
 * Vitest runs with the lib directory as cwd (the vitest.config.ts root), so
 * the bootstrap is resolved relative to it rather than via import.meta.url,
 * which the jsdom environment rewrites to a non-file scheme.
 */
const BOOTSTRAP_SOURCE = readFileSync(
  path.resolve(process.cwd(), '../../trusted-server-core/src/integrations/gpt_bootstrap.js'),
  'utf8'
);

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnyRecord = Record<string, any>;

type TestWindow = Window & {
  googletag?: AnyRecord;
  tsjs?: AnyRecord;
};

function runBootstrap(): void {
  // Evaluate in the jsdom global scope, exactly as an inline <script> would.
  new Function(BOOTSTRAP_SOURCE)();
}

describe('gpt_bootstrap.js fallback', () => {
  let rafQueue: FrameRequestCallback[];
  let readyState: DocumentReadyState;

  function flushFrame(): void {
    const queued = [...rafQueue];
    rafQueue.length = 0;
    queued.forEach((cb) => cb(0));
  }

  beforeEach(() => {
    delete (window as TestWindow).tsjs;
    delete (window as TestWindow).googletag;
    rafQueue = [];
    (
      window as { requestAnimationFrame: typeof window.requestAnimationFrame }
    ).requestAnimationFrame = ((cb: FrameRequestCallback) => {
      rafQueue.push(cb);
      return rafQueue.length;
    }) as typeof window.requestAnimationFrame;
    readyState = 'loading';
    Object.defineProperty(document, 'readyState', {
      configurable: true,
      get: () => readyState,
    });
  });

  afterEach(() => {
    document.body.innerHTML = '';
    delete (document as unknown as Record<string, unknown>).readyState;
    delete (document as unknown as Record<string, unknown>).hidden;
    delete (window as unknown as Record<string, unknown>).requestAnimationFrame;
    delete (window as TestWindow).tsjs;
    delete (window as TestWindow).googletag;
    vi.restoreAllMocks();
  });

  it('installs fallback adInit and scheduleInitialAdInit when the bundle is absent', () => {
    runBootstrap();
    const ts = (window as TestWindow).tsjs!;
    expect(typeof ts.adInit).toBe('function');
    expect(typeof ts.scheduleInitialAdInit).toBe('function');
  });

  it('does not override an already-installed bundle implementation', () => {
    const bundleAdInit = vi.fn();
    (window as TestWindow).tsjs = { adInit: bundleAdInit };
    runBootstrap();
    const ts = (window as TestWindow).tsjs!;
    expect(ts.adInit).toBe(bundleAdInit);
    // The whole bootstrap stands down when the bundle already installed
    // adInit — it must not shadow the bundle's scheduler either.
    expect(ts.scheduleInitialAdInit).toBeUndefined();
  });

  it('fallback scheduler applies the SSR payload and defers adInit past load plus two frames', () => {
    runBootstrap();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    ts.scheduleInitialAdInit!({ atf: { hb_pb: '1.00' } });
    expect(ts.bids).toEqual({ atf: { hb_pb: '1.00' } });
    expect(adInit).not.toHaveBeenCalled();

    window.dispatchEvent(new Event('load'));
    flushFrame();
    expect(adInit).not.toHaveBeenCalled();
    flushFrame();
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('fallback scheduler rides animation frames in a hidden document, holding adInit until first view', () => {
    // Mirrors the bundle scheduler's intended hidden-tab behavior: rAF is not
    // serviced while hidden, so a background-tab load holds the initial
    // adInit until first view — the fallback must not diverge to a timer.
    Object.defineProperty(document, 'hidden', {
      configurable: true,
      get: () => true,
    });
    runBootstrap();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    ts.scheduleInitialAdInit!({ atf: { hb_pb: '1.00' } });
    window.dispatchEvent(new Event('load'));
    expect(rafQueue.length).toBeGreaterThan(0);
    expect(adInit).not.toHaveBeenCalled();

    flushFrame();
    flushFrame();
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('fallback scheduler stands down when a navigation generation has advanced', () => {
    runBootstrap();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;
    ts.bids = { live_slot: { hb_pb: '2.50' } };
    ts.navGeneration = 1;

    ts.scheduleInitialAdInit!({ ssr_slot: { hb_pb: '1.00' } });
    expect(ts.bids).toEqual({ live_slot: { hb_pb: '2.50' } });

    window.dispatchEvent(new Event('load'));
    flushFrame();
    flushFrame();
    expect(adInit).not.toHaveBeenCalled();
  });

  it('fallback adInit defines, targets, and displays a TS slot through the command queue', () => {
    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([]),
      refresh: vi.fn(),
    };
    // The bootstrap's slot-handoff install wraps defineSlot/display/refresh
    // with patched delegating functions, so assertions must target the
    // original spies captured here, not the (re-assigned) googletag members.
    const defineSlot = vi.fn().mockReturnValue(mockSlot);
    const display = vi.fn();
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot,
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
      display,
    };
    document.body.innerHTML =
      '<div id="div-atf-sidebar-container"><div id="div-atf-sidebar"></div></div>';
    runBootstrap();
    const ts = (window as TestWindow).tsjs!;
    ts.adSlots = [
      {
        id: 'atf_sidebar_ad',
        gam_unit_path: '/123/atf',
        div_id: 'div-atf-sidebar',
        formats: [[300, 250]],
      },
    ];
    ts.bids = { atf_sidebar_ad: { hb_pb: '1.00' } };

    ts.adInit!();

    expect(defineSlot).toHaveBeenCalledWith('/123/atf', [[300, 250]], 'div-atf-sidebar');
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_pb', '1.00');
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('ts_initial', '1');
    expect(display).toHaveBeenCalledWith('div-atf-sidebar');
    expect(ts.servicesEnabled).toBe(true);
  });

  it('fallback adInit cancels queued work when the generation advances before the queue drains', () => {
    const commandQueue: Array<() => void> = [];
    // Captured spies: the handoff patcher reassigns pubads.refresh (and
    // googletag.defineSlot/display) to delegating wrappers on drain.
    const refresh = vi.fn();
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([]),
      refresh,
    };
    const defineSlot = vi.fn();
    (window as TestWindow).googletag = {
      cmd: commandQueue,
      defineSlot,
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
      display: vi.fn(),
    };
    document.body.innerHTML = '<div id="div-atf-sidebar"></div>';
    runBootstrap();
    const ts = (window as TestWindow).tsjs!;
    ts.adSlots = [
      {
        id: 'atf_sidebar_ad',
        gam_unit_path: '/123/atf',
        div_id: 'div-atf-sidebar',
        formats: [[300, 250]],
      },
    ];

    // GPT not loaded: adInit's work sits queued.
    ts.adInit!();
    expect(commandQueue.length).toBeGreaterThan(0);

    // A navigation commits (the bundle's SPA hook advances the generation),
    // then GPT loads and drains the queue: the stale work must stand down.
    ts.navGeneration = 1;
    commandQueue.splice(0).forEach((fn) => fn());
    expect(defineSlot).not.toHaveBeenCalled();
    expect(refresh).not.toHaveBeenCalled();
    expect(mockPubads.enableSingleRequest).not.toHaveBeenCalled();
  });
});
