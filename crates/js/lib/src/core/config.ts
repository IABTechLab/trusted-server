// Global configuration storage for the tsjs runtime (mode, logging, etc.).
import { log, LogLevel } from './log';
import type { Config } from './types';
import { RequestMode } from './types';

let CONFIG: Config = { mode: RequestMode.FirstParty };

// Merge publisher-provided config and adjust the log level accordingly.
export function setConfig(cfg: Config): void {
  CONFIG = { ...CONFIG, ...cfg };
  const debugFlag = cfg.debug;
  const l = cfg.logLevel as LogLevel | undefined;
  if (typeof l === 'string') log.setLevel(l);
  else if (debugFlag === true) log.setLevel('debug');
  log.info('setConfig:', cfg);
}

// Return a defensive copy so callers can't mutate shared state.
export function getConfig(): Config {
  return { ...CONFIG };
}
