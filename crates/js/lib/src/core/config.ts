// Global configuration storage for the tsjs runtime (mode, logging, etc.).
import { log, LogLevel } from './log';
import type { Config, GamConfig } from './types';
import { RequestMode } from './types';

let CONFIG: Config = { mode: RequestMode.FirstParty };

// Lazy import to avoid circular dependencies - GAM integration may not be present
let setGamConfigFn: ((cfg: GamConfig) => void) | null | undefined = undefined;

function getSetGamConfig(): ((cfg: GamConfig) => void) | null {
  if (setGamConfigFn === undefined) {
    try {
      // Dynamic import path - bundler will include if gam integration is present
      // eslint-disable-next-line @typescript-eslint/no-require-imports
      const gam = require('../integrations/gam/index');
      setGamConfigFn = gam.setGamConfig || null;
    } catch {
      // GAM integration not available
      setGamConfigFn = null;
    }
  }
  return setGamConfigFn ?? null;
}

// Merge publisher-provided config and adjust the log level accordingly.
export function setConfig(cfg: Config): void {
  CONFIG = { ...CONFIG, ...cfg };
  const debugFlag = cfg.debug;
  const l = cfg.logLevel as LogLevel | undefined;
  if (typeof l === 'string') log.setLevel(l);
  else if (debugFlag === true) log.setLevel('debug');

  // Forward GAM config to the GAM integration if present
  if (cfg.gam) {
    const setGam = getSetGamConfig();
    if (setGam) {
      setGam(cfg.gam);
    }
  }

  log.info('setConfig:', cfg);
}

// Return a defensive copy so callers can't mutate shared state.
export function getConfig(): Config {
  return { ...CONFIG };
}
