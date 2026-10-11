// Shared logger used across tsjs with console output gated by a runtime log level.
export type LogLevel = 'silent' | 'error' | 'warn' | 'info' | 'debug';

const LEVELS: Record<LogLevel, number> = { silent: -1, error: 0, warn: 1, info: 2, debug: 3 };
// Each IIFE carries its own logger code, but all copies share one level.
const LOG_LEVEL_KEY = Symbol.for('trusted-server.logLevel');

type LogGlobal = typeof globalThis & { [LOG_LEVEL_KEY]?: LogLevel };

function getLevel(): LogLevel {
  return ((globalThis as LogGlobal)[LOG_LEVEL_KEY] ??= 'warn');
}

function levelNum(l: LogLevel) {
  return LEVELS[l] ?? 1;
}
function ts(): string {
  try {
    return new Date().toISOString();
  } catch {
    return '';
  }
}
function supportsCss(): boolean {
  try {
    return typeof (globalThis as unknown as { window?: unknown }).window !== 'undefined';
  } catch {
    return false;
  }
}

function styleFor(method: 'log' | 'info' | 'warn' | 'error'): string {
  switch (method) {
    case 'error':
      return 'background:#dc2626;color:#fff;padding:1px 4px;border-radius:2px;font-weight:600';
    case 'warn':
      return 'background:#d97706;color:#fff;padding:1px 4px;border-radius:2px;font-weight:600';
    case 'info':
      return 'background:#2563eb;color:#fff;padding:1px 4px;border-radius:2px;font-weight:600';
    default:
      return 'background:#6b7280;color:#fff;padding:1px 4px;border-radius:2px;font-weight:600';
  }
}

function print(method: 'log' | 'info' | 'warn' | 'error', ...args: unknown[]) {
  const c:
    | Partial<Record<'log' | 'info' | 'warn' | 'error', (...a: unknown[]) => void>>
    | undefined = (globalThis as unknown as { console?: Console }).console;
  if (!c || typeof c[method] !== 'function') return;
  if (supportsCss()) {
    c[method]('%c[tsjs]%c ' + ts() + ':', styleFor(method), 'color:inherit', ...args);
  } else {
    c[method](`[tsjs] ${ts()}:`, ...args);
  }
}

// Thin wrapper around console that keeps timestamped output and honours a runtime log level.
export const log = {
  setLevel(l: LogLevel) {
    (globalThis as LogGlobal)[LOG_LEVEL_KEY] = l;
  },
  getLevel,
  info: (...a: unknown[]) => {
    if (levelNum(getLevel()) >= LEVELS.info) print('info', ...a);
  },
  warn: (...a: unknown[]) => {
    if (levelNum(getLevel()) >= LEVELS.warn) print('warn', ...a);
  },
  error: (...a: unknown[]) => {
    if (levelNum(getLevel()) >= LEVELS.error) print('error', ...a);
  },
  debug: (...a: unknown[]) => {
    if (levelNum(getLevel()) >= LEVELS.debug) print('log', ...a);
  },
};
