export type Size = readonly [number, number];

export interface Banner {
  sizes: ReadonlyArray<Size>;
}

export interface MediaTypes {
  banner?: Banner;
}

export interface AdUnit {
  code: string;
  mediaTypes?: MediaTypes;
}

export interface TsjsApi {
  version: string;
  que: Array<() => void>;
  addAdUnits(units: AdUnit | AdUnit[]): void;
  renderAdUnit(codeOrUnit: string | AdUnit): void;
  renderAllAdUnits(): void;
  setConfig?(cfg: Config): void;
  getConfig?(): Config;
  // Accept Prebid-like signatures: requestBids(opts) or requestBids(callback, opts)
  requestBids?(opts?: RequestBidsOptions): void;
  requestBids?(callback: RequestBidsCallback, opts?: RequestBidsOptions): void;
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

export interface Config {
  debug?: boolean;
  logLevel?: 'silent' | 'error' | 'warn' | 'info' | 'debug';
  /**
   * When true, renderCreativeIntoSlot will create a container div if the
   * target slot id is not found. Defaults to false to avoid injecting ads
   * into unexpected places if the page structure differs.
   */
  autoCreateSlots?: boolean;
  // Extendable for future fields
  [key: string]: unknown;
}

export type RequestBidsCallback = () => void;
export interface RequestBidsOptions {
  bidsBackHandler?: RequestBidsCallback;
  timeout?: number;
}

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
export interface OpenRtbBid {
  impid?: string;
  adm?: string;
  [key: string]: unknown;
}
export interface OpenRtbSeatBid {
  bid?: OpenRtbBid[] | null;
}
export interface OpenRtbBidResponse {
  seatbid?: OpenRtbSeatBid[] | null;
}
