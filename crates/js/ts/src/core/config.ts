import { log, LogLevel } from './log';
import type { Config } from './types';

let CONFIG: Config = {};

export function setConfig(cfg: Config): void {
  CONFIG = { ...CONFIG, ...cfg };
  const debugFlag = cfg.debug;
  const l = cfg.logLevel as LogLevel | undefined;
  if (typeof l === 'string') log.setLevel(l);
  else if (debugFlag === true) log.setLevel('debug');
  log.info('setConfig:', cfg);
}

export function getConfig(): Config {
  return { ...CONFIG };
}
