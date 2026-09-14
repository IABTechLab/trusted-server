import { log } from '../core/log';

export type BootFailureReason = 'abi_mismatch' | 'bundle_partial';

export class TsjsUnavailableError extends Error {
  public readonly code = 'runtime_unavailable' as const;

  public constructor(
    public readonly releaseId: string,
    public readonly reason: BootFailureReason
  ) {
    super('TSJS runtime is unavailable');
    this.name = 'TsjsUnavailableError';
  }
}

const LOG_LEVELS = Object.freeze({
  silent: true,
  error: true,
  warn: true,
  info: true,
  debug: true,
});

function observeLog(callback: () => void): void {
  try {
    callback();
  } catch {
    // The public logger is observation only.
  }
}

export const publicLog = Object.freeze({
  setLevel: (level: Parameters<typeof log.setLevel>[0]) => {
    if (!Object.prototype.hasOwnProperty.call(LOG_LEVELS, level)) {
      throw new TypeError('Invalid TSJS log level');
    }
    log.setLevel(level);
  },
  getLevel: () => log.getLevel(),
  error: (...values: readonly unknown[]) => observeLog(() => log.error(...values)),
  warn: (...values: readonly unknown[]) => observeLog(() => log.warn(...values)),
  info: (...values: readonly unknown[]) => observeLog(() => log.info(...values)),
  debug: (...values: readonly unknown[]) => observeLog(() => log.debug(...values)),
});
