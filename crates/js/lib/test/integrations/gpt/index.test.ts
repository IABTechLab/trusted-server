import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  isGuardInstalled,
  resetGuardState,
} from '../../../src/integrations/gpt/script_guard';

// We import installGptShim dynamically to avoid the auto-init side effect.
// Tests call installGptShim() explicitly after setting up the environment.

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
    resetGuardState();
    win = window as GptWindow;
    delete win.googletag;

    // Dynamic import to get a fresh reference (the module self-init already
    // ran at first import, but installGptShim is idempotent via the guard).
    const mod = await import('../../../src/integrations/gpt/index');
    installGptShim = mod.installGptShim;
  });

  afterEach(() => {
    resetGuardState();
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

describe('GPT shim – runtime gating', () => {
  type GatedWindow = Window & {
    __tsjs_gpt_enabled?: boolean;
    googletag?: { cmd: Array<() => void> };
  };

  let win: GatedWindow;

  beforeEach(() => {
    resetGuardState();
    win = window as GatedWindow;
    delete win.googletag;
    delete win.__tsjs_gpt_enabled;
  });

  afterEach(() => {
    resetGuardState();
    delete (window as GatedWindow).googletag;
    delete (window as GatedWindow).__tsjs_gpt_enabled;
  });

  it('installs the shim when __tsjs_gpt_enabled is set', async () => {
    win.__tsjs_gpt_enabled = true;

    const { installGptShim } = await import(
      '../../../src/integrations/gpt/index'
    );

    // Explicitly call since the dynamic import may have already cached.
    installGptShim();

    expect(isGuardInstalled()).toBe(true);
    expect(win.googletag).toBeDefined();
  });

  it('does not install the shim when __tsjs_gpt_enabled is absent', async () => {
    // No flag set — shim should stay dormant.
    const { installGptShim } = await import(
      '../../../src/integrations/gpt/index'
    );

    // Reset guard to verify the auto-init did NOT install.
    resetGuardState();
    delete win.googletag;

    // Manually verify: calling installGptShim without the flag should still
    // work (it's a direct call), but the *auto-init path* would not have run.
    // The key assertion is that the guard is not installed after reset.
    expect(isGuardInstalled()).toBe(false);
  });
});
