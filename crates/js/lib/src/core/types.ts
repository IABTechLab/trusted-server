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
  setConfig?(cfg: Record<string, unknown>): void;
  getConfig?(): Record<string, unknown>;
  requestAds?(opts?: { bidsBackHandler?: () => void; timeout?: number }): void;
  requestAds?(
    callback: () => void,
    opts?: { bidsBackHandler?: () => void; timeout?: number }
  ): void;
  log?: {
    setLevel(l: 'silent' | 'error' | 'warn' | 'info' | 'debug'): void;
    getLevel(): 'silent' | 'error' | 'warn' | 'info' | 'debug';
    info(...args: unknown[]): void;
    warn(...args: unknown[]): void;
    error(...args: unknown[]): void;
    debug(...args: unknown[]): void;
  };
}
