import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// We import installGptShim dynamically so each test can control whether the
// GPT enable flag is present before module evaluation.

async function importGuardModule() {
  return import('../../../src/integrations/gpt/script_guard');
}

type GptWindow = Window & {
  googletag?: {
    cmd: Array<() => void> & {
      push: (...items: Array<() => void>) => number;
      __tsPushed?: boolean;
    };
    _loaded_?: boolean;
  };
};

describe('GPT shim – patchCommandQueue', () => {
  let win: GptWindow;
  let installGptShim: () => boolean;

  beforeEach(async () => {
    // Reset any prior state
    const guard = await importGuardModule();
    guard.resetGuardState();
    win = window as GptWindow;
    delete win.googletag;

    // Dynamic import to get a fresh reference (the module self-init already
    // ran at first import, but installGptShim is idempotent via the guard).
    const mod = await import('../../../src/integrations/gpt/index');
    installGptShim = mod.installGptShim;
  });

  afterEach(async () => {
    const guard = await importGuardModule();
    guard.resetGuardState();
    delete (window as GptWindow).googletag;
  });

  it('preserves googletag.cmd array identity', () => {
    const originalCmd: Array<() => void> = [];
    win.googletag = { cmd: originalCmd } as GptWindow['googletag'];

    installGptShim();

    expect(win.googletag!.cmd).toBe(originalCmd);
  });

  it('preserves custom cmd.push when GPT is already loaded', () => {
    // Simulate GPT's loaded state: cmd.push executes callbacks immediately.
    const executed: string[] = [];
    const cmd: Array<() => void> = [];
    const gptCustomPush = (...fns: Array<() => void>): number => {
      // GPT's custom push executes immediately and appends to the array.
      for (const fn of fns) {
        fn();
        cmd[cmd.length] = fn;
      }
      return cmd.length;
    };
    cmd.push = gptCustomPush;

    win.googletag = { cmd, _loaded_: true } as GptWindow['googletag'];

    installGptShim();

    // Push a new callback after patching — it should still delegate to
    // GPT's custom push (which executes immediately).
    win.googletag!.cmd.push(() => {
      executed.push('post-patch');
    });

    expect(executed).toContain('post-patch');
  });

  it('wraps callbacks pushed after patching with error handling', () => {
    win.googletag = { cmd: [] } as GptWindow['googletag'];

    installGptShim();

    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    // Push a callback that throws
    win.googletag!.cmd.push(() => {
      throw new Error('test error');
    });

    // The wrapped callback should be in the queue — execute it.
    const wrappedFn = win.googletag!.cmd[win.googletag!.cmd.length - 1];
    expect(() => wrappedFn()).not.toThrow();

    errorSpy.mockRestore();
  });

  it('re-wraps already-queued pending callbacks in place', () => {
    const callOrder: string[] = [];
    const pending = [() => callOrder.push('first'), () => callOrder.push('second')];

    win.googletag = { cmd: pending } as GptWindow['googletag'];

    installGptShim();

    // The pending callbacks should have been wrapped in place.
    // Execute them — they should not throw even if one of them did.
    for (const fn of win.googletag!.cmd) {
      fn();
    }

    expect(callOrder).toEqual(['first', 'second']);
  });

  it('handles pending callback that throws without breaking the queue', () => {
    const callOrder: string[] = [];
    const pending = [
      () => {
        throw new Error('boom');
      },
      () => callOrder.push('after-error'),
    ];

    win.googletag = { cmd: pending } as GptWindow['googletag'];

    installGptShim();

    // Execute all wrapped callbacks — the error should be caught.
    for (const fn of win.googletag!.cmd) {
      expect(() => fn()).not.toThrow();
    }

    expect(callOrder).toEqual(['after-error']);
  });

  it('is idempotent — calling installGptShim twice does not double-wrap', () => {
    const calls: number[] = [];
    win.googletag = { cmd: [] } as GptWindow['googletag'];

    installGptShim();
    const pushAfterFirst = win.googletag!.cmd.push;

    installGptShim();
    const pushAfterSecond = win.googletag!.cmd.push;

    // The push function should be the same reference (not re-wrapped).
    expect(pushAfterSecond).toBe(pushAfterFirst);

    // Push a callback and verify it only executes once (not double-wrapped).
    win.googletag!.cmd.push(() => calls.push(1));
    const fn = win.googletag!.cmd[win.googletag!.cmd.length - 1];
    fn();

    expect(calls).toEqual([1]);
  });

  it('creates googletag.cmd if it does not exist', () => {
    // No googletag at all on window.
    delete win.googletag;

    installGptShim();

    expect(win.googletag).toBeDefined();
    expect(Array.isArray(win.googletag!.cmd)).toBe(true);
  });
});

describe('GPT – installSlimPrebidLoader', () => {
  type SlimWindow = Window & { __tsjs_slim_prebid_url?: string };

  afterEach(() => {
    delete (window as SlimWindow).__tsjs_slim_prebid_url;
  });

  it('is a no-op when __tsjs_slim_prebid_url is not set', async () => {
    const { installSlimPrebidLoader } = await import('../../../src/integrations/gpt/index');
    const addEventListenerSpy = vi.spyOn(window, 'addEventListener');
    installSlimPrebidLoader();
    expect(addEventListenerSpy).not.toHaveBeenCalledWith('load', expect.any(Function));
    addEventListenerSpy.mockRestore();
  });

  it('appends a deferred script tag when __tsjs_slim_prebid_url is set and load fires', async () => {
    (window as SlimWindow).__tsjs_slim_prebid_url = 'https://cdn.example.com/slim-prebid.js';
    const { installSlimPrebidLoader } = await import('../../../src/integrations/gpt/index');

    installSlimPrebidLoader();

    // Simulate the window load event.
    window.dispatchEvent(new Event('load'));

    const scripts = Array.from(document.querySelectorAll('script[defer]'));
    const injected = scripts.find(
      (s) => (s as HTMLScriptElement).src === 'https://cdn.example.com/slim-prebid.js'
    );
    expect(injected).toBeDefined();

    // Clean up
    injected?.parentNode?.removeChild(injected);
  });

  it('module init calls installSlimPrebidLoader — script injected when URL is preset', async () => {
    vi.resetModules();
    (window as SlimWindow).__tsjs_slim_prebid_url = 'https://cdn.example.com/slim-prebid-init.js';

    await import('../../../src/integrations/gpt/index');
    window.dispatchEvent(new Event('load'));

    const scripts = Array.from(document.querySelectorAll('script[defer]'));
    const injected = scripts.find(
      (s) => (s as HTMLScriptElement).src === 'https://cdn.example.com/slim-prebid-init.js'
    );
    expect(injected).toBeDefined();

    injected?.parentNode?.removeChild(injected);
  });
});

describe('GPT – installTsAdInit', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
    delete (window as any).tsjs;
    delete (window as any).googletag;
  });

  afterEach(() => {
    document.body.innerHTML = '';
    delete (window as any).tsjs;
    delete (window as any).googletag;
  });

  it('clears stale TS-managed targeting before applying a new route to a reused GPT slot', async () => {
    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    const slotTargeting = new Map<string, string[]>([
      ['hb_pb', ['1.20']],
      ['hb_bidder', ['kargo']],
      ['hb_adid', ['old-ad']],
      ['hb_cache_host', ['cache.example.com']],
      ['hb_cache_path', ['/cache']],
      ['ts_initial', ['1']],
      ['pos', ['old-pos']],
    ]);
    const clearTargeting = vi.fn((key?: string) => {
      if (key) {
        slotTargeting.delete(key);
      } else {
        slotTargeting.clear();
      }
      return gptSlot;
    });
    const gptSlot: any = {
      getSlotElementId: vi.fn(() => 'div-ad-homepage-header'),
      getTargeting: vi.fn((key: string) => slotTargeting.get(key) ?? []),
      setTargeting: vi.fn((key: string, value: string | string[]) => {
        slotTargeting.set(key, Array.isArray(value) ? value : [value]);
        return gptSlot;
      }),
      clearTargeting,
    };
    const pubads = {
      getSlots: vi.fn(() => [gptSlot]),
      enableSingleRequest: vi.fn(),
      addEventListener: vi.fn(),
      refresh: vi.fn(),
    };
    const cmd: Array<() => void> = [];
    cmd.push = (...callbacks: Array<() => void>) => {
      callbacks.forEach((callback) => callback());
      return cmd.length;
    };

    document.body.innerHTML = '<div id="div-ad-homepage-header"></div>';
    (window as any).googletag = {
      cmd,
      pubads: () => pubads,
      defineSlot: vi.fn(),
      destroySlots: vi.fn(),
      enableServices: vi.fn(),
    };
    (window as any).tsjs = {
      prevSlotTargetingKeys: {
        'div-ad-homepage-header': ['pos'],
      },
      adSlots: [
        {
          id: 'homepage_header_ad',
          gam_unit_path: '/123/homepage',
          div_id: 'div-ad-homepage-header',
          formats: [[728, 90]],
          targeting: { zone: 'homepage' },
        },
      ],
      bids: {},
    };

    installTsAdInit();
    (window as any).tsjs.adInit();

    expect(clearTargeting).toHaveBeenCalledWith('hb_pb');
    expect(clearTargeting).toHaveBeenCalledWith('hb_bidder');
    expect(clearTargeting).toHaveBeenCalledWith('hb_adid');
    expect(clearTargeting).toHaveBeenCalledWith('hb_cache_host');
    expect(clearTargeting).toHaveBeenCalledWith('hb_cache_path');
    expect(clearTargeting).toHaveBeenCalledWith('ts_initial');
    expect(clearTargeting).toHaveBeenCalledWith('pos');
    expect(slotTargeting.get('hb_pb')).toBeUndefined();
    expect(slotTargeting.get('hb_bidder')).toBeUndefined();
    expect(slotTargeting.get('hb_adid')).toBeUndefined();
    expect(slotTargeting.get('hb_cache_host')).toBeUndefined();
    expect(slotTargeting.get('hb_cache_path')).toBeUndefined();
    expect(slotTargeting.get('pos')).toBeUndefined();
    expect(slotTargeting.get('zone')).toEqual(['homepage']);
    expect(slotTargeting.get('ts_initial')).toEqual(['1']);
  });
});

describe('GPT shim – runtime gating', () => {
  type GatedWindow = Window & {
    __tsjs_gpt_enabled?: boolean;
    googletag?: { cmd: Array<() => void> };
  };

  let win: GatedWindow;

  beforeEach(async () => {
    const guard = await importGuardModule();
    guard.resetGuardState();
    win = window as GatedWindow;
    delete win.googletag;
    delete win.__tsjs_gpt_enabled;
  });

  afterEach(async () => {
    const guard = await importGuardModule();
    guard.resetGuardState();
    delete (window as GatedWindow).googletag;
    delete (window as GatedWindow).__tsjs_gpt_enabled;
    delete (window as Record<string, unknown>).__tsjs_installGptShim;
  });

  it('installs the shim when activation function is called (simulates server inline script)', async () => {
    const guard = await importGuardModule();
    const { installGptShim } = await import('../../../src/integrations/gpt/index');

    // Simulate what the server-injected inline script does:
    // set the flag then call the activation function.
    win.__tsjs_gpt_enabled = true;
    installGptShim();

    expect(guard.isGuardInstalled()).toBe(true);
    expect(win.googletag).toBeDefined();
  });

  it('registers __tsjs_installGptShim on window after import', async () => {
    vi.resetModules();
    await import('../../../src/integrations/gpt/index');

    expect(typeof (window as Record<string, unknown>).__tsjs_installGptShim).toBe('function');
  });

  it('auto-installs the shim when the enable flag is set before import', async () => {
    vi.resetModules();
    win.__tsjs_gpt_enabled = true;

    const guard = await importGuardModule();
    await import('../../../src/integrations/gpt/index');

    expect(guard.isGuardInstalled()).toBe(true);
    expect(win.googletag).toBeDefined();
  });

  it('does not install the shim when only imported (no explicit activation)', async () => {
    // Reset modules so the next dynamic import re-evaluates the module.
    vi.resetModules();

    const guard = await importGuardModule();
    // Import a fresh copy — the module should register the activation
    // function on `window` but NOT call `installGptShim()` on its own.
    await import('../../../src/integrations/gpt/index');

    // Assert immediately — the guard must not be installed because the
    // module only registers `__tsjs_installGptShim`, it does not auto-init.
    expect(guard.isGuardInstalled()).toBe(false);
    expect(win.googletag).toBeUndefined();
  });
});

describe('GPT debug ADM iframe hardening', () => {
  it('sandbox token list omits allow-same-origin', async () => {
    const mod = await import('../../../src/integrations/gpt/index');

    expect(mod.ADM_IFRAME_SANDBOX).toContain('allow-scripts');
    // allow-scripts + allow-same-origin on srcdoc content removes the
    // sandbox's origin isolation — the pair must never be reintroduced.
    expect(mod.ADM_IFRAME_SANDBOX).not.toContain('allow-same-origin');
  });

  it('safeAdmIframeSrc accepts http(s), relative, and protocol-relative URLs', async () => {
    const { safeAdmIframeSrc } = await import('../../../src/integrations/gpt/index');

    expect(safeAdmIframeSrc('https://ads.example.com/creative')).toBe(
      'https://ads.example.com/creative'
    );
    expect(safeAdmIframeSrc('http://ads.example.com/creative')).toBe(
      'http://ads.example.com/creative'
    );
    expect(safeAdmIframeSrc('//ads.example.com/creative')).toBe('https://ads.example.com/creative');
    expect(safeAdmIframeSrc('/first-party/creative?sig=abc')).toBe('/first-party/creative?sig=abc');
  });

  it('safeAdmIframeSrc rejects script-executing and opaque schemes', async () => {
    const { safeAdmIframeSrc } = await import('../../../src/integrations/gpt/index');

    expect(safeAdmIframeSrc('javascript:alert(1)')).toBeUndefined();
    expect(safeAdmIframeSrc('data:text/html,<script>alert(1)</script>')).toBeUndefined();
    expect(safeAdmIframeSrc('blob:https://example.com/uuid')).toBeUndefined();
  });
});
