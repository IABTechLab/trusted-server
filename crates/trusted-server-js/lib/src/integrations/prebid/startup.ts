import type {
  PrebidAdapter,
  PrebidEventFacade,
  PrebidTrustedServerAuctionV1,
} from '../../adapters/prebid';
import type { GoogletagPublisherRefreshCall } from '../../adapters/googletag';

interface RefreshPolicyCapability {
  readonly prepare: (
    call: Readonly<GoogletagPublisherRefreshCall>
  ) => PromiseLike<unknown> | undefined;
}

export interface PrebidStartup {
  readonly activate: () => () => void;
  readonly start: (config: unknown) => void;
}

export interface PrebidStartupOptions {
  readonly dispose: () => void;
  readonly onAuction: (auction: Readonly<PrebidTrustedServerAuctionV1>) => void;
  readonly onAuctionEnd: (event: unknown, prebid: Readonly<PrebidEventFacade>) => void;
  readonly prebid: Pick<PrebidAdapter, 'notifyReady' | 'run'>;
  readonly refresh?: Readonly<{
    readonly configure?: (config: unknown) => void;
    readonly install: (policy: RefreshPolicyCapability) => (() => void) | undefined;
    readonly policy: RefreshPolicyCapability & Readonly<{ dispose: () => void }>;
  }>;
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
  let refreshPolicyRelease: (() => void) | undefined;

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
      const refresh = options.refresh;
      if (refresh) {
        try {
          refreshPolicyRelease = refresh.install(refresh.policy);
          if (!refreshPolicyRelease) throw new Error('Prebid refresh policy is unavailable');
        } catch (error) {
          released = true;
          try {
            disposeOwnedOperation(activationOperation, activationEffects);
          } finally {
            try {
              refresh.policy.dispose();
            } finally {
              options.dispose();
            }
          }
          throw error;
        }
      }
      return (): void => {
        if (released) return;
        released = true;
        try {
          disposeOwnedOperation(bidderOperation, bidderEffects);
        } finally {
          try {
            disposeOwnedOperation(activationOperation, activationEffects);
          } finally {
            try {
              options.refresh?.policy.dispose();
            } finally {
              try {
                refreshPolicyRelease?.();
              } finally {
                options.dispose();
              }
            }
          }
        }
      };
    },
    start: (config: unknown): void => {
      if (!activated || released || started) throw new Error('Prebid startup is unavailable');
      started = true;
      try {
        options.refresh?.configure?.(config);
        bidderOperation = options.prebid.run((prebid) =>
          prebid.registerTrustedServerBidder(options.onAuction)
        );
        retainEffects(bidderOperation.result, (release) => {
          bidderEffects = release;
        });
        options.start?.(config);
      } finally {
        options.prebid.notifyReady();
      }
    },
  });
}
