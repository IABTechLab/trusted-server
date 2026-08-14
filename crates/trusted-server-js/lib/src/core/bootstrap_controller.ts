import type { BootFailureReason } from '../kernel/fallback';

const BOOT_DEADLINE_MS = 10_000;

export type BootstrapControllerState =
  'installing' | 'agent_registered' | 'action_started' | 'settled' | 'failed';

export interface BootstrapControllerOptions {
  readonly performance: Readonly<{ mark: (name: string) => void }>;
  readonly now: () => number;
  readonly startedAtMs?: number;
  readonly setTimer: (callback: () => void, delayMs: number) => unknown;
  readonly clearTimer: (handle: unknown) => void;
  readonly onFailure: (reason: BootFailureReason) => void;
}

export interface BootstrapController {
  readonly state: BootstrapControllerState;
  readonly startedAtMs: number;
  readonly registerAgent: () => boolean;
  readonly startAction: () => boolean;
  readonly settle: () => boolean;
  readonly fail: (reason: BootFailureReason) => boolean;
}

/** Own the one protected bootstrap deadline and the preceding bids-script mark. */
export function createBootstrapController(
  options: BootstrapControllerOptions
): BootstrapController {
  const startedAtMs = options.startedAtMs ?? options.now();
  let state: BootstrapControllerState = 'installing';
  let timer: unknown;

  const expired = (): boolean => {
    const elapsed = options.now() - startedAtMs;
    return !Number.isFinite(elapsed) || elapsed >= BOOT_DEADLINE_MS;
  };
  const clearDeadline = (): void => {
    if (timer === undefined) return;
    try {
      options.clearTimer(timer);
    } catch {
      // A hostile timer primitive cannot reopen bootstrap ownership.
    }
    timer = undefined;
  };
  const fail = (reason: BootFailureReason): boolean => {
    if (state === 'settled' || state === 'failed') return false;
    state = 'failed';
    clearDeadline();
    try {
      options.onFailure(reason);
    } catch {
      // Failure publication cannot make the terminal controller live again.
    }
    return true;
  };

  try {
    options.performance.mark('tsjs:bids-script');
  } catch {
    // Timing observability cannot affect the boot transaction.
  }
  const remaining = Math.max(0, BOOT_DEADLINE_MS - (options.now() - startedAtMs));
  timer = options.setTimer(() => fail('bundle_partial'), remaining);

  return Object.freeze({
    get state() {
      return state;
    },
    startedAtMs,
    registerAgent: (): boolean => {
      if (state !== 'installing') return false;
      if (expired()) return fail('bundle_partial') && false;
      state = 'agent_registered';
      return true;
    },
    startAction: (): boolean => {
      if (state !== 'agent_registered') return false;
      if (expired()) return fail('bundle_partial') && false;
      state = 'action_started';
      return true;
    },
    settle: (): boolean => {
      if (state !== 'agent_registered' && state !== 'action_started') return false;
      state = 'settled';
      clearDeadline();
      return true;
    },
    fail,
  });
}
