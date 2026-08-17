import { readFileSync } from 'node:fs';
import path from 'node:path';

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

import type { TsjsApi } from '../../../src/core/types';

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

// The command queue the bootstrap pushes into: a real array once GPT has
// loaded, or the bare `push`-only stub GPT installs before then.
type MockCommandQueue = Array<() => void> | { push: (fn: () => void) => unknown };

// Minimal googletag surface the bootstrap touches.
interface MockGoogleTag {
  cmd: MockCommandQueue;
  defineSlot: (adUnitPath: string, sizes: Array<[number, number]>, divId: string) => unknown;
  pubads: () => unknown;
  enableServices: () => void;
  display: (divId: string) => void;
  getConfig?: (key: string) => Record<string, unknown>;
  setConfig?: (config: Record<string, unknown>) => void;
}

// `tsjs` is declared globally as the full `TsjsApi`; `Omit` drops it from
// `Window` so the fixtures below only have to satisfy the fields they set.
type TestWindow = Omit<Window, 'tsjs'> & {
  googletag?: MockGoogleTag;
  tsjs?: Partial<TsjsApi>;
  __tsjs_gam_attribution_enabled?: boolean;
};

function makeGoogleTag(overrides: Partial<MockGoogleTag> = {}): MockGoogleTag {
  const pubads = {
    getSlots: vi.fn(() => []),
    refresh: vi.fn(),
  };

  return {
    cmd: [],
    defineSlot: vi.fn(),
    pubads: vi.fn(() => pubads),
    enableServices: vi.fn(),
    display: vi.fn(),
    ...overrides,
  };
}

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
    delete (window as TestWindow).__tsjs_gam_attribution_enabled;
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
    delete (window as TestWindow).__tsjs_gam_attribution_enabled;
    vi.restoreAllMocks();
  });

  it('does not create googletag before the guard when attribution is omitted or false', () => {
    for (const flag of [undefined, false]) {
      const bundleAdInit = vi.fn();
      (window as TestWindow).tsjs = { adInit: bundleAdInit };
      delete (window as TestWindow).googletag;
      if (flag === undefined) {
        delete (window as TestWindow).__tsjs_gam_attribution_enabled;
      } else {
        (window as TestWindow).__tsjs_gam_attribution_enabled = flag;
      }

      runBootstrap();

      expect((window as TestWindow).googletag).toBeUndefined();
      expect((window as TestWindow).tsjs!.adInit).toBe(bundleAdInit);
    }
  });

  it('queues string-valued page targeting before a later publisher command', () => {
    const queue: Array<() => void> = [];
    const setConfig = vi.fn();
    (window as TestWindow).googletag = makeGoogleTag({ cmd: queue, setConfig });
    (window as TestWindow).__tsjs_gam_attribution_enabled = true;

    runBootstrap();
    const publisherCommand = vi.fn();
    queue.push(publisherCommand);
    [...queue].forEach((command) => command());

    expect(setConfig).toHaveBeenCalledWith({ targeting: { ts: 'true' } });
    expect(setConfig.mock.invocationCallOrder[0]).toBeLessThan(
      publisherCommand.mock.invocationCallOrder[0]
    );
  });

  it('queues targeting before the preinstalled adInit guard without replacing bundle APIs', () => {
    const queue: Array<() => void> = [];
    const setConfig = vi.fn();
    const bundleAdInit = vi.fn();
    (window as TestWindow).googletag = makeGoogleTag({ cmd: queue, setConfig });
    (window as TestWindow).tsjs = { adInit: bundleAdInit };
    (window as TestWindow).__tsjs_gam_attribution_enabled = true;

    runBootstrap();

    expect(queue).toHaveLength(1);
    expect((window as TestWindow).tsjs!.adInit).toBe(bundleAdInit);
    expect((window as TestWindow).tsjs!.scheduleInitialAdInit).toBeUndefined();
    queue.forEach((command) => command());
    expect(setConfig).toHaveBeenCalledWith({ targeting: { ts: 'true' } });
  });

  it('keeps bootstrap installation working when setConfig is unavailable', () => {
    const queue: Array<() => void> = [];
    (window as TestWindow).googletag = makeGoogleTag({ cmd: queue });
    (window as TestWindow).__tsjs_gam_attribution_enabled = true;

    runBootstrap();

    expect(() => [...queue].forEach((command) => command())).not.toThrow();
    expect(typeof (window as TestWindow).tsjs!.adInit).toBe('function');
    expect(typeof (window as TestWindow).tsjs!.scheduleInitialAdInit).toBe('function');
  });

  it('isolates a throwing setConfig from later publisher commands', () => {
    const queue: Array<() => void> = [];
    const setConfig = vi.fn(() => {
      throw new Error('publisher setConfig failed');
    });
    const disableInitialLoad = vi.fn();
    const pubads = {
      disableInitialLoad,
      getSlots: vi.fn(() => []),
      refresh: vi.fn(),
    };
    const publisherCommand = vi.fn();
    (window as TestWindow).googletag = makeGoogleTag({
      cmd: queue,
      setConfig,
      pubads: vi.fn(() => pubads),
    });
    (window as TestWindow).__tsjs_gam_attribution_enabled = true;

    runBootstrap();
    queue.push(publisherCommand);

    expect(() => [...queue].forEach((command) => command())).not.toThrow();
    expect(setConfig).toHaveBeenCalledWith({ targeting: { ts: 'true' } });
    expect(publisherCommand).toHaveBeenCalledTimes(1);
    expect(typeof (window as TestWindow).tsjs!.adInit).toBe('function');
    pubads.disableInitialLoad();
    expect(disableInitialLoad).toHaveBeenCalledTimes(1);
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(true);
  });

  it('still tracks the wrapped legacy disableInitialLoad path', () => {
    const queue: Array<() => void> = [];
    const disableInitialLoad = vi.fn();
    const pubads = {
      disableInitialLoad,
      getSlots: vi.fn(() => []),
      refresh: vi.fn(),
    };
    (window as TestWindow).googletag = makeGoogleTag({
      cmd: queue,
      pubads: vi.fn(() => pubads),
    });
    (window as TestWindow).__tsjs_gam_attribution_enabled = true;

    runBootstrap();
    [...queue].forEach((command) => command());
    pubads.disableInitialLoad();

    expect(disableInitialLoad).toHaveBeenCalledTimes(1);
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(true);
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

  it('fallback scheduler guards the SSR slot definitions with the same generation check', () => {
    // The shared-template seam hands slots to the scheduler rather than assigning
    // them itself, so the fallback has to honour the same guard as the bundle. If it
    // applied them unconditionally, a page whose bundle failed to load would take the
    // stale SSR slots over a committed navigation's.
    runBootstrap();
    const ts = (window as TestWindow).tsjs!;
    ts.adInit = vi.fn();
    const liveSlot = {
      id: 'live_slot',
      gam_unit_path: '/123/live',
      div_id: 'div-live',
      formats: [[300, 250]] as Array<[number, number]>,
    };
    const ssrSlot = {
      id: 'ssr_slot',
      gam_unit_path: '/123/ssr',
      div_id: 'div-ssr',
      formats: [[728, 90]] as Array<[number, number]>,
    };

    ts.scheduleInitialAdInit!({ ssr_slot: { hb_pb: '1.00' } }, [ssrSlot]);
    expect(ts.adSlots).toEqual([ssrSlot]);

    ts.adSlots = [liveSlot];
    ts.navGeneration = 1;
    ts.scheduleInitialAdInit!({ ssr_slot: { hb_pb: '1.00' } }, [ssrSlot]);
    expect(ts.adSlots).toEqual([liveSlot]);
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
    const nativeRefresh = vi.fn();
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([]),
      refresh: nativeRefresh,
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
    expect(nativeRefresh).not.toHaveBeenCalled();
    expect(mockPubads.enableSingleRequest).not.toHaveBeenCalled();
  });
});
