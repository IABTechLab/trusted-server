import { describe, expect, it, vi } from 'vitest';

import { createPrebidLaterIntegrationRegistration } from '../../../src/integrations/prebid/later';
import type { PrebidRefreshPolicy } from '../../../src/integrations/prebid/refresh';
import { createTestNavigationIdentityIssuer } from '../../../src/kernel/identity';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  PreparedIntegration,
} from '../../../src/kernel/integration_registry';
import { createRuntimeSession } from '../../../src/kernel/sessions';

const RELEASE_ID = 'a'.repeat(64);
const CONFIG = Object.freeze({
  accountId: 'fictional-account',
  timeout: 1_000,
  debug: false,
  bidders: Object.freeze(['server']),
  clientSideBidders: Object.freeze(['client']),
  excludedGamAdUnitPathSuffixes: Object.freeze(['/excluded']),
});

function harness(acceptPolicy = true) {
  const runtime = createRuntimeSession({
    createIdentityIssuer: () =>
      createTestNavigationIdentityIssuer({
        getRandomValues: (target) => {
          target.fill(4);
          return target;
        },
      }),
  });
  const navigationResult = runtime.startInitialNavigation();
  if (!navigationResult.ok) throw new Error('Expected current navigation');
  const navigation = navigationResult.value;
  const physicalSlot = Object.freeze({ id: 'physical-slot' });
  const directAuctionUnit = Object.freeze({
    code: 'slot-one',
    mediaTypes: Object.freeze({
      banner: Object.freeze({ sizes: Object.freeze([Object.freeze([300, 250])]) }),
    }),
    bids: Object.freeze([
      Object.freeze({
        bidder: 'trustedServer',
        params: Object.freeze({
          bidderParams: Object.freeze({
            server: Object.freeze({ placement: 'server-owned' }),
          }),
        }),
      }),
      Object.freeze({ bidder: 'client', params: Object.freeze({ placement: 'publisher-client' }) }),
    ]),
  });
  const clearTargeting = vi.fn();
  const gptOperationDispose = vi.fn();
  const googletagRun = vi.fn((command: (facade: object) => unknown) =>
    Object.freeze({
      result: Promise.resolve(
        command(
          Object.freeze({
            adUnitPath: () => '/network/eligible',
            clearTargeting,
          })
        )
      ),
      dispose: gptOperationDispose,
    })
  );
  const requestBids = vi.fn((request: Readonly<Record<string, unknown>>) => {
    const callback = request.bidsBackHandler;
    if (typeof callback === 'function') callback();
  });
  const setTargetingForGpt = vi.fn();
  const prebidOperationDispose = vi.fn();
  const prebidRun = vi.fn((command: (facade: object) => unknown) =>
    Object.freeze({
      result: Promise.resolve(
        command(
          Object.freeze({
            requestBids,
            setTargetingForGpt,
          })
        )
      ),
      dispose: prebidOperationDispose,
    })
  );
  let policy: PrebidRefreshPolicy | undefined;
  const policyRelease = vi.fn();
  const installRefreshPolicy = vi.fn((candidate: PrebidRefreshPolicy) => {
    policy = candidate;
    return acceptPolicy ? policyRelease : undefined;
  });
  const directAuctionUnitForSlot = vi.fn((slot: object) =>
    slot === physicalSlot ? directAuctionUnit : undefined
  );
  const activationDisposers: Array<() => void> = [];
  const registration = createPrebidLaterIntegrationRegistration(RELEASE_ID);
  const prepared = registration.prepare(
    Object.freeze({
      config: CONFIG,
      interfaces: Object.freeze({
        'runtime.v1': Object.freeze({}),
        'slots.v1': Object.freeze({}),
        'gpt.v1': Object.freeze({
          adapter: Object.freeze({ run: googletagRun }),
          directAuctionUnitForSlot,
          installRefreshPolicy,
          navigation: () => navigation,
        }),
        'prebid.v1': Object.freeze({
          adapter: Object.freeze({ run: prebidRun }),
          clientSideBidders: CONFIG.clientSideBidders,
          excludedGamAdUnitPathSuffixes: CONFIG.excludedGamAdUnitPathSuffixes,
        }),
      }),
      onDispose: () => undefined,
      signal: new AbortController().signal,
    } satisfies IntegrationPrepareContext)
  ) as PreparedIntegration;
  const activation = Object.freeze({
    afterCommit: vi.fn(),
    onDispose: (callback: () => void) => activationDisposers.push(callback),
    signal: new AbortController().signal,
  } satisfies IntegrationActivationContext);

  return {
    activation,
    activationDisposers,
    clearTargeting,
    directAuctionUnitForSlot,
    googletagRun,
    installRefreshPolicy,
    physicalSlot,
    policy: () => policy,
    policyRelease,
    prebidRun,
    registration,
    requestBids,
    runtime,
    setTargetingForGpt,
    prepared,
  };
}

describe('RCJ-PREBID-04 deferred refresh ownership', () => {
  it('has no initial-admission effect and runs one detached refresh after activation', async () => {
    const owner = harness();

    expect(owner.registration).toMatchObject({ id: 'prebid_later', phase: 'deferred' });
    expect(owner.installRefreshPolicy).not.toHaveBeenCalled();
    expect(owner.googletagRun).not.toHaveBeenCalled();
    expect(owner.prebidRun).not.toHaveBeenCalled();
    expect(owner.directAuctionUnitForSlot).not.toHaveBeenCalled();

    owner.prepared.activate(owner.activation);
    expect(owner.installRefreshPolicy).toHaveBeenCalledOnce();
    expect(owner.prebidRun).not.toHaveBeenCalled();

    const completion = owner
      .policy()
      ?.prepare(Object.freeze({ slots: Object.freeze([owner.physicalSlot]) }));
    expect(completion).toBeDefined();
    await Promise.resolve(completion);

    expect(owner.googletagRun).toHaveBeenCalledOnce();
    expect(owner.directAuctionUnitForSlot).toHaveBeenCalledExactlyOnceWith(owner.physicalSlot);
    expect(owner.prebidRun).toHaveBeenCalledOnce();
    expect(owner.requestBids).toHaveBeenCalledOnce();
    expect(owner.setTargetingForGpt).toHaveBeenCalledExactlyOnceWith(['slot-one']);
    expect(owner.requestBids.mock.calls[0]?.[0]).toMatchObject({
      adUnits: [
        {
          code: 'slot-one',
          bids: [
            {
              bidder: 'trustedServer',
              params: { bidderParams: { server: { placement: 'server-owned' } } },
            },
            { bidder: 'client', params: { placement: 'publisher-client' } },
          ],
        },
      ],
    });

    owner.activationDisposers.reverse().forEach((dispose) => dispose());
    expect(owner.policyRelease).toHaveBeenCalledOnce();
    owner.runtime.dispose();
  });

  it('fails closed when GPT already has a refresh owner without starting another auction', () => {
    const owner = harness(false);

    expect(() => owner.prepared.activate(owner.activation)).toThrow(
      'Prebid refresh policy is duplicated'
    );
    expect(owner.installRefreshPolicy).toHaveBeenCalledOnce();
    expect(owner.googletagRun).not.toHaveBeenCalled();
    expect(owner.prebidRun).not.toHaveBeenCalled();

    owner.activationDisposers.reverse().forEach((dispose) => dispose());
    expect(owner.policyRelease).not.toHaveBeenCalled();
    owner.runtime.dispose();
  });
});
