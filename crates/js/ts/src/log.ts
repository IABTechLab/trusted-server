export type LogLevel = 'silent' | 'error' | 'warn' | 'info' | 'debug'

const LEVELS: Record<LogLevel, number> = { silent: -1, error: 0, warn: 1, info: 2, debug: 3 }
let currentLevel: LogLevel = 'warn'

function levelNum(l: LogLevel) {
  return LEVELS[l] ?? 1
}
function ts(): string {
  try {
    return new Date().toISOString()
  } catch {
    return ''
  }
}
function print(method: 'log' | 'info' | 'warn' | 'error', ...args: unknown[]) {
  const c: Partial<Record<'log' | 'info' | 'warn' | 'error', (...a: unknown[]) => void>> | undefined =
    (globalThis as unknown as { console?: Console }).console
  if (c && typeof c[method] === 'function') c[method](`[tsjs] ${ts()}:`, ...args)
}

export const log = {
  setLevel(l: LogLevel) {
    currentLevel = l
  },
  getLevel(): LogLevel {
    return currentLevel
  },
  info: (...a: unknown[]) => {
    if (levelNum(currentLevel) >= LEVELS.info) print('info', ...a)
  },
  warn: (...a: unknown[]) => {
    if (levelNum(currentLevel) >= LEVELS.warn) print('warn', ...a)
  },
  error: (...a: unknown[]) => {
    if (levelNum(currentLevel) >= LEVELS.error) print('error', ...a)
  },
  debug: (...a: unknown[]) => {
    if (levelNum(currentLevel) >= LEVELS.debug) print('log', ...a)
  },
}
