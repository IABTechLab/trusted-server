import { describe, expect, it, vi } from 'vitest';

import type {
  PrebidAdapter,
  PrebidEventFacade,
  PrebidFacade,
  PrebidTrustedServerAuctionV1,
} from '../../../src/adapters/prebid';
import { createPrebidStartup } from '../../../src/integrations/prebid/startup';
import type { GptRefreshPolicy } from '../../../src/integrations/gpt/startup';

describe('Prebid startup bridge', () => {
  it('installs one reversible bidder/event operation before starting the external boundary', async () => {
    let bidderListener: ((auction: Readonly<PrebidTrustedServerAuctionV1>) => void) | undefined;
    let auctionEndListener:
      ((event: unknown, prebid: Readonly<PrebidEventFacade>) => void) | undefined;
    const operationDispose = vi.fn();
    const releaseBidder = vi.fn();
    const releaseAuctionEnd = vi.fn();
    const order: string[] = [];
    const eventFacade = Object.freeze({ highestBids: vi.fn(() => Object.freeze([])) });
    const facade = Object.freeze({
      registerTrustedServerBidder: vi.fn(
        (listener: (auction: Readonly<PrebidTrustedServerAuctionV1>) => void) => {
          order.push('register-bidder');
          bidderListener = listener;
          return () => {
            order.push('release-bidder');
            releaseBidder();
          };
        }
      ),
      subscribe: vi.fn(
        (
          eventType: string,
          listener: (event: unknown, prebid: Readonly<PrebidEventFacade>) => void
        ) => {
          expect(eventType).toBe('auctionEnd');
          order.push('subscribe-auction-end');
          auctionEndListener = listener;
          return () => {
            order.push('release-auction-end');
            releaseAuctionEnd();
          };
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
    expect(order).toEqual(['subscribe-auction-end']);
    expect(facade.registerTrustedServerBidder).not.toHaveBeenCalled();
    expect(facade.subscribe).toHaveBeenCalledTimes(1);
    const event = Object.freeze({ auctionId: 'auction-one' });
    auctionEndListener?.(event, eventFacade);
    expect(onAuctionEnd).toHaveBeenCalledExactlyOnceWith(event, eventFacade);

    const config = Object.freeze({ externalBundleUrl: '/prebid.js' });
    startup.start(config);
    await Promise.resolve();
    expect(start).toHaveBeenCalledExactlyOnceWith(config);
    expect(notifyReady).toHaveBeenCalledTimes(1);
    expect(run).toHaveBeenCalledTimes(2);
    expect(facade.registerTrustedServerBidder).toHaveBeenCalledTimes(1);
    expect(order).toEqual(['subscribe-auction-end', 'register-bidder']);
    const auction = Object.freeze({
      auctionId: 'auction-one',
      bids: Object.freeze([]),
      complete: vi.fn(),
    });
    bidderListener?.(auction);
    expect(onAuction).toHaveBeenCalledExactlyOnceWith(auction);

    release();
    release();
    expect(operationDispose).toHaveBeenCalledTimes(2);
    expect(releaseAuctionEnd).toHaveBeenCalledTimes(1);
    expect(releaseBidder).toHaveBeenCalledTimes(1);
    expect(order).toEqual([
      'subscribe-auction-end',
      'register-bidder',
      'release-bidder',
      'release-auction-end',
    ]);
    expect(dispose).toHaveBeenCalledTimes(1);
  });

  it('releases effects that settle after the runtime owner is already disposed', async () => {
    let resolveOperation!: (release: () => void) => void;
    const result = new Promise<() => void>((resolve) => {
      resolveOperation = resolve;
    });
    const operationDispose = vi.fn();
    const run = vi.fn(() =>
      Object.freeze({ status: 'present' as const, result, dispose: operationDispose })
    );
    const dispose = vi.fn();
    const startup = createPrebidStartup({
      dispose,
      onAuction: vi.fn(),
      onAuctionEnd: vi.fn(),
      prebid: Object.freeze({ run, notifyReady: vi.fn() }) as unknown as PrebidAdapter,
    });
    const releaseEffects = vi.fn();

    const release = startup.activate();
    release();
    resolveOperation(releaseEffects);
    await Promise.resolve();

    expect(operationDispose).toHaveBeenCalledTimes(1);
    expect(releaseEffects).toHaveBeenCalledTimes(1);
    expect(dispose).toHaveBeenCalledTimes(1);
  });

  it('installs the TS auctionEnd listener before startup can add a publisher callback', async () => {
    const listeners: Array<(event: unknown, prebid: Readonly<PrebidEventFacade>) => void> = [];
    const order: string[] = [];
    const eventFacade = Object.freeze({ highestBids: vi.fn(() => Object.freeze([])) });
    const facade = Object.freeze({
      registerTrustedServerBidder: vi.fn(() => vi.fn()),
      subscribe: vi.fn(
        (
          eventType: string,
          listener: (event: unknown, prebid: Readonly<PrebidEventFacade>) => void
        ) => {
          expect(eventType).toBe('auctionEnd');
          listeners.push(listener);
          return vi.fn();
        }
      ),
    }) as unknown as Readonly<PrebidFacade>;
    const run = vi.fn((command: (prebid: Readonly<PrebidFacade>) => unknown) =>
      Object.freeze({
        status: 'present' as const,
        result: Promise.resolve(command(facade)),
        dispose: vi.fn(),
      })
    );
    const startup = createPrebidStartup({
      dispose: vi.fn(),
      onAuction: vi.fn(),
      onAuctionEnd: () => order.push('trusted-server'),
      prebid: Object.freeze({ run, notifyReady: vi.fn() }) as unknown as PrebidAdapter,
      start: () => {
        listeners.push(() => order.push('publisher'));
      },
    });

    startup.activate();
    await Promise.resolve();
    startup.start(Object.freeze({}));
    await Promise.resolve();
    const event = Object.freeze({ auctionId: 'auction-one' });
    for (const listener of listeners) listener(event, eventFacade);

    expect(order).toEqual(['trusted-server', 'publisher']);
  });

  it('installs, configures, and releases one runtime-owned GPT refresh policy', async () => {
    const order: string[] = [];
    const operationDispose = vi.fn();
    const facade = Object.freeze({
      registerTrustedServerBidder: vi.fn(() => vi.fn()),
      subscribe: vi.fn(() => vi.fn()),
    }) as unknown as Readonly<PrebidFacade>;
    const prebid = Object.freeze({
      notifyReady: vi.fn(),
      run: vi.fn((command: (prebid: Readonly<PrebidFacade>) => unknown) =>
        Object.freeze({
          status: 'present' as const,
          result: Promise.resolve(command(facade)),
          dispose: operationDispose,
        })
      ),
    }) as unknown as Pick<PrebidAdapter, 'notifyReady' | 'run'>;
    const policy = Object.freeze({ prepare: vi.fn(), dispose: vi.fn() });
    const releasePolicy = vi.fn(() => order.push('release-policy'));
    const install = vi.fn((_policy: GptRefreshPolicy) => {
      order.push('install-policy');
      return releasePolicy;
    });
    const configure = vi.fn((_config: unknown) => order.push('configure-policy'));
    const start = vi.fn(() => order.push('start-prebid'));
    const startup = createPrebidStartup({
      dispose: vi.fn(),
      onAuction: vi.fn(),
      onAuctionEnd: vi.fn(),
      prebid,
      refresh: Object.freeze({ configure, install, policy }),
      start,
    });

    const release = startup.activate();
    expect(install).toHaveBeenCalledExactlyOnceWith(policy);
    const config = Object.freeze({ excludedGamAdUnitPathSuffixes: Object.freeze(['/skip']) });
    startup.start(config);
    expect(configure).toHaveBeenCalledExactlyOnceWith(config);
    expect(order).toEqual(['install-policy', 'configure-policy', 'start-prebid']);

    release();
    release();
    expect(policy.dispose).toHaveBeenCalledOnce();
    expect(releasePolicy).toHaveBeenCalledOnce();
  });

  it('unwinds the adapter and policy when GPT refuses a second refresh owner', () => {
    const operationDispose = vi.fn();
    const prebid = Object.freeze({
      notifyReady: vi.fn(),
      run: vi.fn(() =>
        Object.freeze({
          status: 'present' as const,
          result: Promise.resolve(vi.fn()),
          dispose: operationDispose,
        })
      ),
    }) as unknown as Pick<PrebidAdapter, 'notifyReady' | 'run'>;
    const policy = Object.freeze({ prepare: vi.fn(), dispose: vi.fn() });
    const dispose = vi.fn();
    const startup = createPrebidStartup({
      dispose,
      onAuction: vi.fn(),
      onAuctionEnd: vi.fn(),
      prebid,
      refresh: Object.freeze({
        install: vi.fn(() => undefined),
        policy,
      }),
    });

    expect(() => startup.activate()).toThrow('Prebid refresh policy is unavailable');
    expect(operationDispose).toHaveBeenCalledOnce();
    expect(policy.dispose).toHaveBeenCalledOnce();
    expect(dispose).toHaveBeenCalledOnce();
  });
});
