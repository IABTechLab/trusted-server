import { describe, expect, it, vi } from 'vitest';

import type {
  PrebidAdapter,
  PrebidEventFacade,
  PrebidFacade,
  PrebidTrustedServerAuctionV1,
} from '../../../src/adapters/prebid';
import { createPrebidStartup } from '../../../src/integrations/prebid/startup';

describe('Prebid startup bridge', () => {
  it('installs one reversible bidder/event operation before starting the external boundary', async () => {
    let bidderListener: ((auction: Readonly<PrebidTrustedServerAuctionV1>) => void) | undefined;
    let auctionEndListener:
      ((event: unknown, prebid: Readonly<PrebidEventFacade>) => void) | undefined;
    const operationDispose = vi.fn();
    const eventFacade = Object.freeze({ highestBids: vi.fn(() => Object.freeze([])) });
    const facade = Object.freeze({
      registerTrustedServerBidder: vi.fn(
        (listener: (auction: Readonly<PrebidTrustedServerAuctionV1>) => void) => {
          bidderListener = listener;
        }
      ),
      subscribe: vi.fn(
        (
          eventType: string,
          listener: (event: unknown, prebid: Readonly<PrebidEventFacade>) => void
        ) => {
          expect(eventType).toBe('auctionEnd');
          auctionEndListener = listener;
          return vi.fn();
        }
      ),
    }) as unknown as Readonly<PrebidFacade>;
    const run = vi.fn((command: (prebid: Readonly<PrebidFacade>) => unknown) =>
      Object.freeze({
        status: 'present' as const,
        result: Promise.resolve(command(facade)),
        dispose: operationDispose,
      })
    );
    const notifyReady = vi.fn();
    const adapter = Object.freeze({ run, notifyReady }) as unknown as PrebidAdapter;
    const onAuction = vi.fn();
    const onAuctionEnd = vi.fn();
    const dispose = vi.fn();
    const start = vi.fn();
    const startup = createPrebidStartup({
      dispose,
      onAuction,
      onAuctionEnd,
      prebid: adapter,
      start,
    });

    const release = startup.activate();
    await Promise.resolve();

    expect(run).toHaveBeenCalledTimes(1);
    expect(facade.registerTrustedServerBidder).toHaveBeenCalledTimes(1);
    expect(facade.subscribe).toHaveBeenCalledTimes(1);
    const auction = Object.freeze({
      auctionId: 'auction-one',
      bids: Object.freeze([]),
      complete: vi.fn(),
    });
    bidderListener?.(auction);
    expect(onAuction).toHaveBeenCalledExactlyOnceWith(auction);
    const event = Object.freeze({ auctionId: 'auction-one' });
    auctionEndListener?.(event, eventFacade);
    expect(onAuctionEnd).toHaveBeenCalledExactlyOnceWith(event, eventFacade);

    const config = Object.freeze({ externalBundleUrl: '/prebid.js' });
    startup.start(config);
    expect(start).toHaveBeenCalledExactlyOnceWith(config);
    expect(notifyReady).toHaveBeenCalledTimes(1);

    release();
    release();
    expect(operationDispose).toHaveBeenCalledTimes(1);
    expect(dispose).toHaveBeenCalledTimes(1);
  });
});
