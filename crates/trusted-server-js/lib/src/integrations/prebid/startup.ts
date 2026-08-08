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

/** Join the private bidder and early winner observer to one reversible runtime owner. */
export function createPrebidStartup(options: PrebidStartupOptions): PrebidStartup {
  let activated = false;
  let released = false;
  let started = false;
  let activationOperation: ReturnType<PrebidStartupOptions['prebid']['run']> | undefined;
  let activationEffects: (() => void) | undefined;
  let bidderOperation: ReturnType<PrebidStartupOptions['prebid']['run']> | undefined;
  let bidderEffects: (() => void) | undefined;

  const retainEffects = (
    result: Promise<unknown>,
    publish: (release: () => void) => void
  ): void => {
    void result.then(
      (candidate) => {
        if (typeof candidate !== 'function') return;
        if (released) candidate();
        else publish(candidate as () => void);
      },
      () => undefined
    );
  };

  const disposeOwnedOperation = (
    operation: ReturnType<PrebidStartupOptions['prebid']['run']> | undefined,
    releaseEffects: (() => void) | undefined
  ): void => {
    try {
      operation?.dispose();
    } finally {
      releaseEffects?.();
    }
  };

  return Object.freeze({
    activate: (): (() => void) => {
      if (activated || released) throw new Error('Prebid startup is already activated');
      activated = true;
      activationOperation = options.prebid.run((prebid) => {
        const releaseAuctionEnd = prebid.subscribe('auctionEnd', options.onAuctionEnd);
        return releaseAuctionEnd;
      });
      retainEffects(activationOperation.result, (release) => {
        activationEffects = release;
      });
      return (): void => {
        if (released) return;
        released = true;
        try {
          disposeOwnedOperation(bidderOperation, bidderEffects);
        } finally {
          try {
            disposeOwnedOperation(activationOperation, activationEffects);
          } finally {
            options.dispose();
          }
        }
      };
    },
    start: (config: unknown): void => {
      if (!activated || released || started) throw new Error('Prebid startup is unavailable');
      started = true;
      bidderOperation = options.prebid.run((prebid) =>
        prebid.registerTrustedServerBidder(options.onAuction)
      );
      retainEffects(bidderOperation.result, (release) => {
        bidderEffects = release;
      });
      try {
        options.start?.(config);
      } finally {
        options.prebid.notifyReady();
      }
    },
  });
}
