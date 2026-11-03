export interface GoogleTagSlot {
  getAdUnitPath(): string;
  getSlotElementId(): string;
  setTargeting(key: string, value: string | string[]): GoogleTagSlot;
  clearTargeting?(key?: string): GoogleTagSlot;
  addService(service: GoogleTagPubAdsService): GoogleTagSlot;
  getTargeting?(key: string): string[];
}

export interface SlotRenderEndedEvent {
  isEmpty: boolean;
  slot: GoogleTagSlot;
}

export interface GoogleTagRefreshOptions {
  changeCorrelator?: boolean;
}

export interface GoogleTagPubAdsService {
  setTargeting(key: string, value: string | string[]): GoogleTagPubAdsService;
  getTargeting(key: string): string[];
  enableSingleRequest(): void;
  addEventListener(event: string, fn: (e: SlotRenderEndedEvent) => void): void;
  refresh(slots?: GoogleTagSlot[], options?: GoogleTagRefreshOptions): void;
  getSlots?(): GoogleTagSlot[];
  disableInitialLoad?(): void;
}

export interface GoogleTagConfig extends Record<string, unknown> {
  disableInitialLoad?: boolean | null;
}

export interface GoogleTagEffectiveConfig {
  disableInitialLoad?: boolean;
}

export type GoogleTagDisplayTarget = string | Element | GoogleTagSlot;

export interface GoogleTag {
  cmd: Array<() => void>;
  pubads(): GoogleTagPubAdsService;
  defineSlot(
    adUnitPath: string,
    size: Array<number | number[]>,
    elementId?: string
  ): GoogleTagSlot | null;
  destroySlots(slots?: GoogleTagSlot[]): boolean;
  enableServices(): void;
  display(target: GoogleTagDisplayTarget): void;
  setConfig?(config: GoogleTagConfig): void;
  getConfig?(keys: string | string[]): GoogleTagEffectiveConfig | undefined;
  _loaded_?: boolean;
}

/** Early IIFE owns native interception and attribution for this document. */
export interface EarlyGptInstallation {
  installShim(): boolean;
  installPageTargeting(): void;
}

export type GptWindow = Window & {
  __tsjs_gpt_early?: EarlyGptInstallation;
  googletag?: Partial<GoogleTag>;
  __tsjs_slim_prebid_url?: string;
};
