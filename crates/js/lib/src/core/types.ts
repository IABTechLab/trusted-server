// Shared TypeScript types for the tsjs core API and extensions.
export type Size = readonly [number, number];

export type LogLevel = 'silent' | 'error' | 'warn' | 'info' | 'debug';

export interface Banner {
  sizes: ReadonlyArray<Size>;
}

export interface MediaTypes {
  banner?: Banner;
}

export interface Bid {
  bidder: string;
  params?: Record<string, unknown>;
}

export interface AdUnit {
  code: string;
  mediaTypes?: MediaTypes;
  bids?: Bid[];
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

/** GAM interceptor configuration. */
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

// Core-neutral request types
export type RequestAdsCallback = () => void;
export interface RequestAdsOptions {
  bidsBackHandler?: RequestAdsCallback;
  timeout?: number;
}

// Back-compat aliases for Prebid-style naming (used by the extension shim)
export type RequestBidsCallback = RequestAdsCallback;

export interface HighestCpmBid {
  adUnitCode: string;
  width: number;
  height: number;
  cpm: number;
  currency: string;
  bidderCode: string;
  creativeId: string;
  adserverTargeting: Record<string, string>;
}

// Minimal OpenRTB response typing
// OpenRTB response typing is specific to the Prebid extension and lives in src/ext/types.ts
