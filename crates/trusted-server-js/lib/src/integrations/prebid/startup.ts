import type {
  PrebidAdapter,
  PrebidEventFacade,
  PrebidTrustedServerAuctionV1,
} from '../../adapters/prebid';

export interface PrebidStartup {
  readonly activate: () => () => void;
  readonly start: (config: unknown) => void;
}

export interface PrebidStartupOptions {
  readonly dispose: () => void;
  readonly onAuction: (auction: Readonly<PrebidTrustedServerAuctionV1>) => void;
  readonly onAuctionEnd: (event: unknown, prebid: Readonly<PrebidEventFacade>) => void;
  readonly prebid: Pick<PrebidAdapter, 'notifyReady' | 'run'>;
  readonly start?: (config: unknown) => void;
}

/** Join the version-pinned Prebid callbacks to runtime-owned publication and selection state. */
export function createPrebidStartup(options: PrebidStartupOptions): PrebidStartup {
  return Object.freeze({
    activate: (): (() => void) => {
      const operation = options.prebid.run((prebid) => {
        prebid.subscribe('auctionEnd', options.onAuctionEnd);
        prebid.registerTrustedServerBidder(options.onAuction);
      });
      void operation.result.catch(() => undefined);
      let active = true;
      return (): void => {
        if (!active) return;
        active = false;
        try {
          operation.dispose();
        } finally {
          options.dispose();
        }
      };
    },
    start: (config: unknown): void => {
      options.start?.(config);
      options.prebid.notifyReady();
    },
  });
}
