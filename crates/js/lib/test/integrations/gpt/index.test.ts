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

type TsAdInitWindow = Window & {
  __ts_ad_slots?: Array<Record<string, unknown>>;
  __ts_request_id?: string;
  googletag?: {
    cmd: Array<() => void>;
    defineSlot: ReturnType<typeof vi.fn>;
    pubads: ReturnType<typeof vi.fn>;
    enableServices: ReturnType<typeof vi.fn>;
    display: ReturnType<typeof vi.fn>;
  };
};

type MockSlot = {
  id: string;
  targeting: Map<string, string[]>;
  setTargeting: ReturnType<typeof vi.fn>;
  getTargeting: ReturnType<typeof vi.fn>;
  addService: ReturnType<typeof vi.fn>;
};

const flushPromises = async () => {
  for (let i = 0; i < 5; i++) {
    await Promise.resolve();
  }
};

function jsonResponse(body: unknown) {
  return {
    json: () => Promise.resolve(body),
  };
}

function createGptHarness(win: TsAdInitWindow) {
  const operations: string[] = [];
  const slots: MockSlot[] = [];
  let slotRenderEnded: ((event: { slot: MockSlot }) => void) | undefined;

  const pubadsService = {
    addEventListener: vi.fn((eventName: string, callback: (event: { slot: MockSlot }) => void) => {
      if (eventName === 'slotRenderEnded') {
        slotRenderEnded = callback;
      }
    }),
    refresh: vi.fn(() => {
      operations.push('refresh');
    }),
  };

  win.googletag = {
    cmd: [],
    defineSlot: vi.fn((_adUnitPath: string, _sizes: unknown, elementId: string) => {
      const targeting = new Map<string, string[]>();
      const slot: MockSlot = {
        id: elementId,
        targeting,
        setTargeting: vi.fn((key: string, value: string | string[]) => {
          const values = Array.isArray(value) ? value : [value];
          targeting.set(key, values);
          operations.push(`set:${elementId}:${key}:${values.join(',')}`);
          return slot;
        }),
        getTargeting: vi.fn((key: string) => targeting.get(key) ?? []),
        addService: vi.fn(() => slot),
      };
      slots.push(slot);
      operations.push(`define:${elementId}`);
      return slot;
    }),
    pubads: vi.fn(() => pubadsService),
    enableServices: vi.fn(() => {
      operations.push('enableServices');
    }),
    display: vi.fn((elementId: string) => {
      operations.push(`display:${elementId}`);
    }),
  };

  return {
    operations,
    pubadsService,
    slots,
    triggerSlotRenderEnded: (slot: MockSlot) => slotRenderEnded?.({ slot }),
  };
}

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

describe('GPT shim – __tsAdInit bootstrap', () => {
  let win: TsAdInitWindow;
  let installTsAdInit: () => boolean;
  let originalFetch: typeof globalThis.fetch;
  let originalSendBeacon: typeof navigator.sendBeacon;

  beforeEach(async () => {
    vi.resetModules();
    win = window as TsAdInitWindow;
    delete win.__ts_ad_slots;
    delete win.__ts_request_id;
    delete (win as TsAdInitWindow & { __tsAdInit?: () => boolean }).__tsAdInit;
    delete (win as TsAdInitWindow & { __tsAdInitInstalled?: boolean }).__tsAdInitInstalled;
    delete win.googletag;
    originalFetch = globalThis.fetch;
    originalSendBeacon = navigator.sendBeacon;

    const mod = await import('../../../src/integrations/gpt/index');
    installTsAdInit = mod.installTsAdInit;
  });

  afterEach(() => {
    globalThis.fetch = originalFetch;
    navigator.sendBeacon = originalSendBeacon;
    delete win.__ts_ad_slots;
    delete win.__ts_request_id;
    delete (win as TsAdInitWindow & { __tsAdInit?: () => boolean }).__tsAdInit;
    delete (win as TsAdInitWindow & { __tsAdInitInstalled?: boolean }).__tsAdInitInstalled;
    delete win.googletag;
  });

  it('fetches request scoped bids without credentials', async () => {
    win.__ts_ad_slots = [];
    win.__ts_request_id = 'request id/1';
    globalThis.fetch = vi.fn(() => Promise.resolve(jsonResponse({}))) as unknown as typeof fetch;
    createGptHarness(win);

    installTsAdInit();
    win.googletag!.cmd[0]();
    await flushPromises();

    expect(globalThis.fetch).toHaveBeenCalledWith('/ts-bids?rid=request%20id%2F1', {
      credentials: 'omit',
    });
  });

  it('applies static slot targeting before refresh', async () => {
    win.__ts_ad_slots = [
      {
        id: 'atf_sidebar',
        gam_unit_path: '/21765378893/atf_sidebar',
        div_id: 'div-atf-sidebar',
        formats: [[300, 250]],
        targeting: { pos: 'atf' },
      },
    ];
    win.__ts_request_id = 'rid-123';
    globalThis.fetch = vi.fn(() => Promise.resolve(jsonResponse({}))) as unknown as typeof fetch;
    const { operations } = createGptHarness(win);

    installTsAdInit();
    win.googletag!.cmd[0]();
    await flushPromises();

    expect(operations.indexOf('set:div-atf-sidebar:pos:atf')).toBeGreaterThanOrEqual(0);
    expect(operations.indexOf('set:div-atf-sidebar:pos:atf')).toBeLessThan(
      operations.indexOf('refresh')
    );
  });

  it('applies hb targeting before refresh', async () => {
    win.__ts_ad_slots = [
      {
        id: 'atf_sidebar',
        gam_unit_path: '/21765378893/atf_sidebar',
        div_id: 'div-atf-sidebar',
        formats: [[300, 250]],
        targeting: {},
      },
    ];
    win.__ts_request_id = 'rid-123';
    globalThis.fetch = vi.fn(() =>
      Promise.resolve(
        jsonResponse({
          atf_sidebar: {
            hb_pb: '1.20',
            hb_bidder: 'rubicon',
            hb_adid: 'ad-123',
          },
        })
      )
    ) as unknown as typeof fetch;
    const { operations } = createGptHarness(win);

    installTsAdInit();
    win.googletag!.cmd[0]();
    await flushPromises();

    for (const field of ['hb_pb', 'hb_bidder', 'hb_adid']) {
      const operation = operations.find((entry) =>
        entry.startsWith(`set:div-atf-sidebar:${field}:`)
      );
      expect(operation).toBeDefined();
      expect(operations.indexOf(operation!)).toBeLessThan(operations.indexOf('refresh'));
    }
  });

  it('refreshes GPT slots when bid fetch fails', async () => {
    win.__ts_ad_slots = [
      {
        id: 'atf_sidebar',
        gam_unit_path: '/21765378893/atf_sidebar',
        div_id: 'div-atf-sidebar',
        formats: [[300, 250]],
        targeting: {},
      },
    ];
    win.__ts_request_id = 'rid-123';
    globalThis.fetch = vi.fn(() =>
      Promise.reject(new Error('network down'))
    ) as unknown as typeof fetch;
    const { pubadsService } = createGptHarness(win);

    installTsAdInit();
    win.googletag!.cmd[0]();
    await flushPromises();

    expect(pubadsService.refresh).toHaveBeenCalledTimes(1);
  });

  it('fires burl only after rendered slot targeting matches bid hb_adid', async () => {
    win.__ts_ad_slots = [
      {
        id: 'atf_sidebar',
        gam_unit_path: '/21765378893/atf_sidebar',
        div_id: 'div-atf-sidebar',
        formats: [[300, 250]],
        targeting: {},
      },
    ];
    win.__ts_request_id = 'rid-123';
    globalThis.fetch = vi.fn(() =>
      Promise.resolve(
        jsonResponse({
          atf_sidebar: {
            hb_pb: '1.20',
            hb_bidder: 'rubicon',
            hb_adid: 'ad-123',
            burl: 'https://bidder.example/bill',
          },
        })
      )
    ) as unknown as typeof fetch;
    navigator.sendBeacon = vi.fn(() => true);
    const { slots, triggerSlotRenderEnded } = createGptHarness(win);

    installTsAdInit();
    win.googletag!.cmd[0]();
    await flushPromises();

    slots[0].targeting.set('hb_adid', ['other-ad']);
    triggerSlotRenderEnded(slots[0]);
    expect(navigator.sendBeacon).not.toHaveBeenCalled();

    slots[0].targeting.set('hb_adid', ['ad-123']);
    triggerSlotRenderEnded(slots[0]);
    expect(navigator.sendBeacon).toHaveBeenCalledWith('https://bidder.example/bill');
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
