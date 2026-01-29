// Shared TypeScript types for the tsjs core API and extensions.
export type Size = readonly [number, number];

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
  que: Array<() => void>;
  addAdUnits(units: AdUnit | AdUnit[]): void;
  renderAdUnit(codeOrUnit: string | AdUnit): void;
  renderAllAdUnits(): void;
  setConfig?(cfg: Config): void;
  getConfig?(): Config;
  // Core API: requestAds; accepts same signatures as Prebid's requestBids
  requestAds?(opts?: RequestAdsOptions): void;
  requestAds?(callback: RequestAdsCallback, opts?: RequestAdsOptions): void;
  getHighestCpmBids?(adUnitCodes?: string | string[]): ReadonlyArray<HighestCpmBid>;
  log?: {
    setLevel(l: 'silent' | 'error' | 'warn' | 'info' | 'debug'): void;
    getLevel(): 'silent' | 'error' | 'warn' | 'info' | 'debug';
    info(...args: unknown[]): void;
    warn(...args: unknown[]): void;
    error(...args: unknown[]): void;
    debug(...args: unknown[]): void;
  };
}

export enum RequestMode {
  FirstParty = 'firstParty',
  ThirdParty = 'thirdParty',
}

export interface Config {
  debug?: boolean;
  logLevel?: 'silent' | 'error' | 'warn' | 'info' | 'debug';
  /** Select ad serving mode. Default is RequestMode.FirstParty. */
  mode?: RequestMode;
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
