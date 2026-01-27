// Shared TypeScript types for the tsjs core API.

export type LogLevel = 'silent' | 'error' | 'warn' | 'info' | 'debug';

export interface GamConfig {
  /** Enable the GAM interceptor. Defaults to false. */
  enabled?: boolean;
  /** Only intercept bids from these bidders. Empty array = all bidders. */
  bidders?: string[];
  /** Force render Prebid creative even if GAM returned a line item. Defaults to false. */
  forceRender?: boolean;
}

export interface Config {
  debug?: boolean;
  logLevel?: LogLevel;
  /** Select ad serving mode: 'render' or 'auction'. */
  mode?: 'render' | 'auction';
  /** GAM interceptor configuration. */
  gam?: GamConfig;
  // Extendable for future fields
  [key: string]: unknown;
}

export interface TsjsApi {
  version: string;
  setConfig(cfg: Config): void;
  getConfig(): Config;
  log: {
    setLevel(l: LogLevel): void;
    getLevel(): LogLevel;
    info(...args: unknown[]): void;
    warn(...args: unknown[]): void;
    error(...args: unknown[]): void;
    debug(...args: unknown[]): void;
  };
}
