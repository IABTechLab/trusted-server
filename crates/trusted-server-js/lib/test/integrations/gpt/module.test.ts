import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  adoptInitialGptDiagnosticsFromHandoff,
  adoptInitialGptFactsFromHandoff,
  adoptInitialGptSlotsFromHandoff,
  adoptInitialPucTicketsFromHandoff,
  installPbsCacheBridge,
  publishGptWinner,
  publishInitialGptProjection,
  startGptSlotOperation,
  type GptWinnerPublicationInput,
  type GptSlotOperationInput,
} from '../../../src/integrations/gpt/module';
import type { BrowserAuctionProjectionV1 } from '../../../src/core/types';
import { createLegacyGptRegistrationForTest as createGptIntegrationRegistration } from '../../helpers/legacy_gpt_registration';
import { createGptIntegrationRegistration as createProductionGptRegistration } from '../../../src/integrations/gpt/module';
import { createRenderRuntimeIntegrationRegistration } from '../../../src/integrations/render_runtime/module';
import type { RuntimeCapabilityV1 } from '../../../src/kernel/runtime';
import { createNoopGoogletagAdapter, type GoogletagFacade } from '../../../src/adapters/googletag';
import { isGuardInstalled, resetGuardState } from '../../../src/integrations/gpt/script_guard';
import { createTestNavigationIdentityIssuer } from '../../../src/kernel/identity';
import {
  createIntegrationRegistry,
  type IntegrationInstallCallbacks,
  type IntegrationRegistration,
} from '../../../src/kernel/integration_registry';
import { createRuntimeSession } from '../../../src/kernel/sessions';
import {
  createCommittedArtifactStore,
  createRenderAttempt,
  createSlotOperation,
  type CommittedRenderArtifact,
  type RenderAttempt,
} from '../../../src/services/render';
import { createTargetingService } from '../../../src/services/targeting';
import { createReservationService } from '../../../src/services/reservations';
import type { SlotRequestOutcome } from '../../../src/services/slots';

const RELEASE_ID = 'a'.repeat(64);
const RESERVATION_ID = `r1_${'a'.repeat(22)}`;
const GPT_CONFIG = Object.freeze({ gamAttributionEnabled: false, pageBidsEnabled: true });

describe('GPT first-display diagnostics adoption', () => {
  it('hydrates exact physical-slot cycle state before slot adoption', () => {
    const physicalSlot = {};
    const adoptDiagnosticsState = vi.fn(() => true);
    const adoption = Object.freeze({
      version: 1 as const,
      adoptInitialDisplay: true as const,
      handoff: Object.freeze({
        artifacts: Object.freeze([]),
        cycles: Object.freeze([
          Object.freeze({
            nextCycleOrdinal: 3,
            records: Object.freeze([
              Object.freeze({
                ordinal: 1,
                responseIdentifier: 'response-one',
                seen: Object.freeze(['slotRequested', 'slotRenderEnded'] as const),
                state: 'completed' as const,
              }),
            ]),
            token: 'gt1_4',
            unknownPriorCycle: false,
          }),
        ]),
        trace: Object.freeze({ nextGlobalSlotOrdinal: 7 }),
      }),
      identities: Object.freeze([physicalSlot]),
    });

    expect(adoptInitialGptDiagnosticsFromHandoff(adoption, { adoptDiagnosticsState })).toBe(
      adoption
    );
    expect(adoptDiagnosticsState).toHaveBeenCalledWith({
      nextTraceTokenOrdinal: 7,
      slots: [
        {
          nextCycleOrdinal: 3,
          physicalSlot,
          records: [
            {
              ordinal: 1,
              responseIdentifier: 'response-one',
              seen: ['slotRequested', 'slotRenderEnded'],
              state: 'completed',
            },
          ],
          traceToken: 'gt1_4',
          unknownPriorCycle: false,
        },
      ],
    });
  });

  it('restores diagnostics facts only when the diagnostics owner is selected', () => {
    const adoptFirstDisplay = vi.fn(
      (_diagnostics: unknown, _resolve?: (token: string) => unknown) => true
    );
    const physicalSlot = {};
    const diagnosticsToken = Object.freeze(Object.create(null) as object);
    const fact = Object.freeze({
      version: 1 as const,
      event: 'slotRenderEnded' as const,
      token: 'gt1_1',
      runtimeSlotNumber: 1,
      cycleOrdinal: 1,
      disposition: 'matched' as const,
      issueReason: null,
      capturedAtMs: 5,
      elementId: 'slot-1',
      adUnitPath: '/123/slot-1',
      isEmpty: false,
      renderedSize: Object.freeze([300, 250] as const),
      isBackfill: false,
      slotContentChanged: true,
      visibilityPercent: null,
    });
    const diagnostics = Object.freeze({
      facts: Object.freeze([fact]),
      overflowCount: 3,
      dropCount: 2,
    });
    const adapter = {
      diagnosticsIdentity: (slot: object) =>
        slot === physicalSlot
          ? Object.freeze({
              token: diagnosticsToken,
              traceToken: 'gt1_1' as never,
              runtimeSlotNumber: 1,
              cycleOrdinal: 1 as never,
            })
          : undefined,
    };
    const adoption = Object.freeze({
      version: 1 as const,
      adoptInitialDisplay: true as const,
      handoff: Object.freeze({
        artifacts: Object.freeze([]),
        cycles: Object.freeze([Object.freeze({ token: 'gt1_1' })]),
        gptDiagnostics: diagnostics,
      }),
      identities: Object.freeze([physicalSlot]),
    });

    expect(adoptInitialGptFactsFromHandoff(adoption, { adoptFirstDisplay }, adapter)).toBe(
      adoption
    );
    expect(adoptFirstDisplay).toHaveBeenCalledWith(diagnostics, expect.any(Function));
    const resolve = adoptFirstDisplay.mock.calls[0]?.[1] as (token: string) => unknown;
    expect(resolve('gt1_1')).toMatchObject({ token: diagnosticsToken, runtimeSlotNumber: 1 });
    expect(adoptInitialGptFactsFromHandoff(adoption, undefined, adapter)).toBeUndefined();
  });

  it('transfers only lifecycle-ticket tombstones to the persistent PUC owner', () => {
    const adoptFirstDisplayTickets = vi.fn(() => true);
    const adoption = Object.freeze({
      version: 1 as const,
      adoptInitialDisplay: true as const,
      handoff: Object.freeze({
        highWater: Object.freeze({ reservationClockEpochMs: 40, nextTicketOrdinal: 8 }),
        tombstones: Object.freeze([
          Object.freeze({ expiresAtMs: 80, kind: 'ticket', value: `t1_${'a'.repeat(22)}` }),
          Object.freeze({ expiresAtMs: 90, kind: 'reservation', value: RESERVATION_ID }),
        ]),
      }),
      identities: Object.freeze([]),
    });

    expect(adoptInitialPucTicketsFromHandoff(adoption, { adoptFirstDisplayTickets })).toBe(
      adoption
    );
    expect(adoptFirstDisplayTickets).toHaveBeenCalledWith({
      clockEpochMs: 40,
      nextTicketOrdinal: 8,
      tombstones: [{ expiresAtMs: 80, ticket: `t1_${'a'.repeat(22)}` }],
    });
  });
});

function createAttemptHarness() {
  const runtime = createRuntimeSession({
    createIdentityIssuer: () =>
      createTestNavigationIdentityIssuer({
        getRandomValues: (target) => {
          target.fill(1);
          return target;
        },
      }),
  });
  const navigationResult = runtime.startInitialNavigation();
  if (!navigationResult.ok) throw new Error('Expected navigation creation');
  const batch = navigationResult.value.createAuctionBatch('gpt-cycle');
  if (!batch) throw new Error('Expected batch creation');
  const artifacts = createCommittedArtifactStore();
  const reservations = createReservationService({
    prepareRenderSource: (candidate) =>
      typeof candidate === 'object' &&
      candidate !== null &&
      Object.isFrozen(candidate) &&
      'type' in candidate &&
      'version' in candidate
        ? (candidate as Readonly<{ type: 'aps' | 'adm'; version: 1 }>)
        : undefined,
  });
  const createAttemptWithOwner = (parentAttemptId?: string) => {
    const owner = batch.createRenderAttempt('slot-one');
    if (!owner.ok) throw new Error(`Expected attempt owner: ${owner.reason}`);
    const created = createRenderAttempt({
      artifacts,
      owner: owner.value,
      prepareRenderSource: (candidate) =>
        typeof candidate === 'object' &&
        candidate !== null &&
        Object.isFrozen(candidate) &&
        'type' in candidate &&
        'version' in candidate
          ? (candidate as Readonly<{ type: 'aps' | 'adm'; version: 1 }>)
          : undefined,
      reservations,
      ...(parentAttemptId === undefined ? {} : { parentAttemptId }),
    });
    if (!created.ok) throw new Error(`Expected render attempt: ${created.reason}`);
    return { attempt: created.value, owner: owner.value };
  };
  const primaryCreated = createAttemptWithOwner();
  const primary = primaryCreated.attempt;
  const artifact = Object.freeze({
    kind: 'puc' as const,
    attemptId: primary.id,
    slot: primary.slot,
    navigationGeneration: primary.navigationGeneration,
    dispose: vi.fn(),
  }) satisfies CommittedRenderArtifact;
  return {
    artifact,
    createAttempt: (parentAttemptId: string): RenderAttempt =>
      createAttemptWithOwner(parentAttemptId).attempt,
    navigation: navigationResult.value,
    primary,
    primaryOwner: primaryCreated.owner,
    reservations,
    runtime,
  };
}

function deferredSlotOutcome() {
  let resolve!: (outcome: SlotRequestOutcome) => void;
  const result = new Promise<SlotRequestOutcome>((resolveResult) => {
    resolve = resolveResult;
  });
  const dispose = vi.fn();
  return {
    dispose,
    request: vi.fn(() => Object.freeze({ status: 'active' as const, result, dispose })),
    resolve,
  };
}

function manifest(ids: readonly string[]) {
  return {
    version: 1,
    releaseId: RELEASE_ID,
    firstDisplay: null,
    runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`,
    integrations: ids.map((id) => ({ id, phase: 'takeover' as const })),
  };
}

function registration(
  id: string,
  prepare: IntegrationRegistration['prepare']
): IntegrationRegistration {
  return Object.freeze({
    abi: 1,
    id,
    phase: 'takeover',
    releaseId: RELEASE_ID,
    prepareSync: () => Object.freeze({ activate: () => undefined }),
    prepare,
  });
}

function callbacks(order: string[]): IntegrationInstallCallbacks {
  return {
    activateCore: () => order.push('core'),
    publish: () => order.push('publish'),
    drainPreload: () => order.push('drain'),
  };
}

function pbsCacheProjection(
  sources: readonly Readonly<{
    cacheHost: string;
    cacheId: string;
    cachePath: string;
    divId: string;
    slot: string;
  }>[]
): Readonly<BrowserAuctionProjectionV1> {
  return Object.freeze({
    version: 1 as const,
    auction: Object.freeze({
      version: 1 as const,
      auctionId: 'pbs-cache-auction',
      results: Object.freeze(
        sources.map((source, index) =>
          Object.freeze({
            slot: source.slot,
            outcome: 'winner' as const,
            candidateId: `CACHEBID${String(index).padStart(4, '0')}`,
          })
        )
      ),
    }),
    slots: Object.freeze(
      sources.map((source) =>
        Object.freeze({
          slot: source.slot,
          gamUnitPath: `/123/${source.slot}`,
          divId: source.divId,
          formats: Object.freeze([Object.freeze([300, 250] as const)]),
          targeting: Object.freeze({}),
        })
      )
    ),
    bids: Object.freeze(
      sources.map((source, index) =>
        Object.freeze({
          candidateId: `CACHEBID${String(index).padStart(4, '0')}`,
          slot: source.slot,
          provider: 'prebid',
          upstreamBidId: `upstream-${index}`,
          cpm: 1.25,
          currency: 'USD' as const,
          targeting: Object.freeze({}),
          renderSource: Object.freeze({
            type: 'pbs_cache' as const,
            version: 1 as const,
            cacheId: source.cacheId,
            cacheHost: source.cacheHost,
            cachePath: source.cachePath,
            width: 300,
            height: 250,
          }),
        })
      )
    ),
  }) as unknown as Readonly<BrowserAuctionProjectionV1>;
}

function pbsCacheSourceFrame(divId: string): HTMLIFrameElement {
  const root = document.createElement('div');
  root.id = divId;
  root.style.width = '1px';
  root.style.height = '1px';
  const frame = document.createElement('iframe');
  frame.setAttribute('width', '1');
  frame.setAttribute('height', '1');
  frame.style.width = '1px';
  frame.style.height = '1px';
  root.appendChild(frame);
  document.body.appendChild(root);
  return frame;
}

function startPbsCacheBridge(projection: Readonly<BrowserAuctionProjectionV1>) {
  const runtime = createRuntimeSession({
    createIdentityIssuer: () =>
      createTestNavigationIdentityIssuer({
        getRandomValues: (target) => {
          target.fill(3);
          return target;
        },
      }),
  });
  const navigation = runtime.startInitialNavigation(projection);
  if (!navigation.ok) throw new Error('Expected PBS Cache test navigation');
  const observe = vi.fn(() => true);
  const release = installPbsCacheBridge(
    document,
    Object.freeze({
      navigation: navigation.value,
      projection,
      session: runtime,
    }),
    () => true,
    observe
  );
  return { navigation: navigation.value, observe, release, runtime };
}

function dispatchPbsCacheRequest(
  source: Window,
  adId: string,
  postMessage: ReturnType<typeof vi.fn>
): ReturnType<typeof vi.spyOn> {
  const event = new MessageEvent('message', {
    data: JSON.stringify({ message: 'Prebid Request', adId }),
    ports: [Object.freeze({ postMessage }) as unknown as MessagePort],
    source,
  });
  const stopped = vi.spyOn(event, 'stopImmediatePropagation');
  window.dispatchEvent(event);
  return stopped;
}

describe('GPT-owned PBS Cache bridge', () => {
  const sharedCacheId = 'shared cache/id';

  afterEach(() => {
    vi.unstubAllGlobals();
    document.getElementById('cache-slot-one')?.remove();
    document.getElementById('cache-slot-two')?.remove();
    document.getElementById('foreign-cache-slot')?.remove();
  });

  it('binds duplicate cache ids to the requesting slot and preserves current-main parse, macro, and resize behavior', async () => {
    const projection = pbsCacheProjection([
      {
        cacheId: sharedCacheId,
        cacheHost: 'first-cache.example',
        cachePath: '/first',
        divId: 'cache-slot-one',
        slot: 'slot-one',
      },
      {
        cacheId: sharedCacheId,
        cacheHost: 'second-cache.example:8443',
        cachePath: '/opaque%2Fpath',
        divId: 'cache-slot-two',
        slot: 'slot-two',
      },
    ]);
    const first = pbsCacheSourceFrame('cache-slot-one');
    const second = pbsCacheSourceFrame('cache-slot-two');
    const foreign = pbsCacheSourceFrame('foreign-cache-slot');
    const fetchCache = vi.fn(async () => ({
      ok: true,
      status: 200,
      text: async () =>
        JSON.stringify({
          adm: '<main data-price="${AUCTION_PRICE}" data-b64="${AUCTION_PRICE:B64}">cached</main>',
          width: 320,
          height: 100,
          price: 2.75,
        }),
    }));
    vi.stubGlobal('fetch', fetchCache);
    const bridge = startPbsCacheBridge(projection);
    const foreignPost = vi.fn();
    const foreignStopped = dispatchPbsCacheRequest(
      foreign.contentWindow!,
      sharedCacheId,
      foreignPost
    );
    expect(foreignStopped).not.toHaveBeenCalled();
    expect(fetchCache).not.toHaveBeenCalled();

    const postMessage = vi.fn();
    const stopped = dispatchPbsCacheRequest(second.contentWindow!, sharedCacheId, postMessage);
    await vi.waitFor(() => expect(postMessage).toHaveBeenCalledOnce());
    expect(stopped).toHaveBeenCalledOnce();
    expect(fetchCache).toHaveBeenCalledExactlyOnceWith(
      'https://second-cache.example:8443/opaque%2Fpath?uuid=shared%20cache%2Fid',
      { mode: 'cors' }
    );
    expect(JSON.parse(String(postMessage.mock.calls[0]?.[0]))).toEqual({
      message: 'Prebid Response',
      adId: sharedCacheId,
      ad: '<main data-price="2.75" data-b64="${AUCTION_PRICE:B64}">cached</main>',
      renderer: expect.any(String),
      width: 320,
      height: 100,
    });
    expect(second.style.width).toBe('320px');
    expect(second.style.height).toBe('100px');
    expect(first.style.width).toBe('1px');
    expect(bridge.observe).not.toHaveBeenCalled();
    bridge.release();
    bridge.runtime.dispose();
  });

  it('keeps fetch, payload, and response-post failures typed and suppresses duplicate in-flight work', async () => {
    const projection = pbsCacheProjection([
      {
        cacheId: sharedCacheId,
        cacheHost: 'cache.example',
        cachePath: '/pbc/v1/cache',
        divId: 'cache-slot-one',
        slot: 'slot-one',
      },
    ]);
    const frame = pbsCacheSourceFrame('cache-slot-one');
    let resolveResponse!: (
      response: Readonly<{ ok: boolean; status: number; text: () => Promise<string> }>
    ) => void;
    const fetchCache = vi.fn(
      () =>
        new Promise<Readonly<{ ok: boolean; status: number; text: () => Promise<string> }>>(
          (resolve) => {
            resolveResponse = resolve;
          }
        )
    );
    vi.stubGlobal('fetch', fetchCache);
    const bridge = startPbsCacheBridge(projection);
    const firstPost = vi.fn();
    dispatchPbsCacheRequest(frame.contentWindow!, sharedCacheId, firstPost);
    dispatchPbsCacheRequest(frame.contentWindow!, sharedCacheId, vi.fn());
    expect(fetchCache).toHaveBeenCalledOnce();
    resolveResponse({ ok: true, status: 200, text: async () => '{"not_adm":true}' });
    await vi.waitFor(() =>
      expect(bridge.observe).toHaveBeenCalledWith(
        Object.freeze({
          kind: 'pbs_cache_bridge',
          slotId: 'slot-one',
          reason: 'invalid_cache_payload',
        })
      )
    );
    expect(firstPost).not.toHaveBeenCalled();

    fetchCache.mockResolvedValueOnce({ ok: false, status: 503, text: async () => '' });
    dispatchPbsCacheRequest(frame.contentWindow!, sharedCacheId, vi.fn());
    await vi.waitFor(() =>
      expect(bridge.observe).toHaveBeenCalledWith(
        Object.freeze({
          kind: 'pbs_cache_bridge',
          slotId: 'slot-one',
          reason: 'cache_fetch_failed',
        })
      )
    );

    fetchCache.mockResolvedValueOnce({
      ok: true,
      status: 200,
      text: async () => '<main>raw cached creative</main>',
    });
    const throwingPost = vi.fn(() => {
      throw new Error('closed port');
    });
    dispatchPbsCacheRequest(frame.contentWindow!, sharedCacheId, throwingPost);
    await vi.waitFor(() =>
      expect(bridge.observe).toHaveBeenCalledWith(
        Object.freeze({
          kind: 'pbs_cache_bridge',
          slotId: 'slot-one',
          reason: 'response_post_failed',
        })
      )
    );
    expect(frame.style.width).toBe('1px');
    bridge.release();
    bridge.runtime.dispose();
  });

  it('makes late cache completion and post-disposal messages inert', async () => {
    const projection = pbsCacheProjection([
      {
        cacheId: sharedCacheId,
        cacheHost: 'cache.example',
        cachePath: '/pbc/v1/cache',
        divId: 'cache-slot-one',
        slot: 'slot-one',
      },
    ]);
    const frame = pbsCacheSourceFrame('cache-slot-one');
    let resolveResponse!: (
      response: Readonly<{ ok: boolean; status: number; text: () => Promise<string> }>
    ) => void;
    const fetchCache = vi.fn(
      () =>
        new Promise<Readonly<{ ok: boolean; status: number; text: () => Promise<string> }>>(
          (resolve) => {
            resolveResponse = resolve;
          }
        )
    );
    vi.stubGlobal('fetch', fetchCache);
    const bridge = startPbsCacheBridge(projection);
    const postMessage = vi.fn();
    dispatchPbsCacheRequest(frame.contentWindow!, sharedCacheId, postMessage);
    expect(fetchCache).toHaveBeenCalledOnce();
    expect(bridge.runtime.replaceNavigation()).toMatchObject({ ok: true });
    resolveResponse({
      ok: true,
      status: 200,
      text: async () => JSON.stringify({ adm: '<main>late</main>' }),
    });
    await Promise.resolve();
    await Promise.resolve();
    expect(postMessage).not.toHaveBeenCalled();
    expect(bridge.observe).not.toHaveBeenCalled();

    bridge.release();
    dispatchPbsCacheRequest(frame.contentWindow!, sharedCacheId, vi.fn());
    expect(fetchCache).toHaveBeenCalledOnce();
    bridge.runtime.dispose();
  });
});

describe('transactional GPT integration module', () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    resetGuardState();
    delete (window as Window & { googletag?: unknown }).googletag;
    document.getElementById('takeover-slot')?.remove();
    document.getElementById('spa-winner')?.remove();
  });

  it('adopts exact first-display GPT identities without issuing a second GPT action', () => {
    const physicalSlot = {};
    const frame = {};
    const navigationGeneration = {};
    const committedArtifact: CommittedRenderArtifact = Object.freeze({
      attemptId: `a1_${'d'.repeat(22)}`,
      dispose: vi.fn(),
      kind: 'puc',
      navigationGeneration,
      slot: 'slot-1',
    });
    const artifactStore = createCommittedArtifactStore();
    expect(artifactStore.promote(committedArtifact)).toBe(true);
    const adoptCommittedArtifact = vi.fn(() => true);
    const adoptGptSlot = vi.fn(() => Object.freeze({ ok: true as const }));
    const adoptRegistrationHighWater = vi.fn(() => true);
    const adoption = Object.freeze({
      version: 1 as const,
      adoptInitialDisplay: true as const,
      handoff: Object.freeze({
        highWater: Object.freeze({ nextSlotRegistrationOrdinal: 2 }),
        slots: Object.freeze([
          Object.freeze({
            id: 'slot-1',
            owner: 'trusted_server',
            domId: 'div-1',
            gamPath: '/123/slot-1',
            formats: Object.freeze([Object.freeze([300, 250])]),
            targetingOwnership: Object.freeze([]),
          }),
        ]),
        cycles: Object.freeze([Object.freeze({ slotId: 'slot-1' })]),
        artifacts: Object.freeze([Object.freeze({ slotId: 'slot-1' })]),
      }),
      identities: Object.freeze([physicalSlot, frame]),
    });

    expect(
      adoptInitialGptSlotsFromHandoff(
        adoption,
        navigationGeneration,
        {
          adoptCommittedArtifact,
          adoptGptSlot,
          adoptRegistrationHighWater,
        },
        artifactStore,
        {
          adopt: vi.fn(),
          observePublisherMutations: vi.fn(),
        },
        {} as never
      )
    ).toBe(adoption);
    expect(adoptRegistrationHighWater).toHaveBeenCalledExactlyOnceWith(navigationGeneration, 2);
    expect(adoptGptSlot).toHaveBeenCalledExactlyOnceWith(navigationGeneration, 'slot-1', {
      definition: {
        adUnitPath: '/123/slot-1',
        elementId: 'div-1',
        sizes: [[300, 250]],
      },
      ownership: 'trusted_server',
      slot: physicalSlot,
    });
    expect(adoptCommittedArtifact).toHaveBeenCalledExactlyOnceWith(
      navigationGeneration,
      'slot-1',
      committedArtifact,
      expect.any(Function)
    );
  });

  it.each(['unchanged', 'same_publisher_write', 'different_publisher_write'] as const)(
    'transfers first-display targeting ownership and preserves %s semantics',
    (mutation) => {
      const physicalSlot = {};
      const frame = {};
      const navigationGeneration = {};
      const values = new Map<string, readonly string[]>([['hb_adid', ['trusted']]]);
      const setTargeting = vi.fn((slot: object, key: string, value: string | readonly string[]) => {
        expect(slot).toBe(physicalSlot);
        values.set(key, Object.freeze(typeof value === 'string' ? [value] : [...value]));
      });
      const clearTargeting = vi.fn((_slot: object, key?: string) => {
        if (key === undefined) values.clear();
        else values.delete(key);
      });
      let publisherObserver:
        Readonly<{ beforePublisherMutation: (slot: object, key?: string) => void }> | undefined;
      const releaseObservation = Object.assign(vi.fn(), { isCurrent: () => true });
      const facade = {
        clearTargeting,
        getTargeting: (_slot: object, key: string) => Object.freeze([...(values.get(key) ?? [])]),
        observeTargeting: (
          slot: object,
          observer: Readonly<{ beforePublisherMutation: (slot: object, key?: string) => void }>
        ) => {
          expect(slot).toBe(physicalSlot);
          publisherObserver = observer;
          return releaseObservation;
        },
        setTargeting,
      };
      const adapter = {
        run: (command: (gpt: typeof facade) => unknown) => {
          const result = command(facade);
          return Object.freeze({
            status: 'present' as const,
            result: Promise.resolve(result),
            dispose: vi.fn(),
          });
        },
      } as never;
      const artifact: CommittedRenderArtifact = Object.freeze({
        attemptId: `a1_${'e'.repeat(22)}`,
        dispose: vi.fn(),
        kind: 'aps_mount',
        navigationGeneration,
        slot: 'slot-1',
      });
      const artifactStore = createCommittedArtifactStore();
      expect(artifactStore.promote(artifact)).toBe(true);
      let retireTargeting: (() => void) | undefined;
      const service = {
        adoptCommittedArtifact: vi.fn(
          (
            _generation: object,
            _slotId: string,
            _artifact: CommittedRenderArtifact,
            onRetire?: () => void
          ) => {
            retireTargeting = onRetire;
            return true;
          }
        ),
        adoptGptSlot: vi.fn(() => Object.freeze({ ok: true as const })),
        adoptRegistrationHighWater: vi.fn(() => true),
      };
      const adoption = Object.freeze({
        version: 1 as const,
        adoptInitialDisplay: true as const,
        handoff: Object.freeze({
          highWater: Object.freeze({ nextSlotRegistrationOrdinal: 2 }),
          slots: Object.freeze([
            Object.freeze({
              id: 'slot-1',
              owner: 'publisher' as const,
              targetingOwnership: Object.freeze([
                Object.freeze({
                  installed: 'trusted',
                  key: 'hb_adid',
                  prior: Object.freeze(['publisher-original']),
                }),
              ]),
            }),
          ]),
          cycles: Object.freeze([Object.freeze({ slotId: 'slot-1' })]),
          artifacts: Object.freeze([Object.freeze({ slotId: 'slot-1' })]),
        }),
        identities: Object.freeze([physicalSlot, frame]),
      });
      const targeting = createTargetingService();

      expect(
        adoptInitialGptSlotsFromHandoff(
          adoption,
          navigationGeneration,
          service,
          artifactStore,
          targeting,
          adapter
        )
      ).toBe(adoption);
      expect(setTargeting).not.toHaveBeenCalled();

      if (mutation !== 'unchanged') {
        publisherObserver?.beforePublisherMutation(physicalSlot, 'hb_adid');
        values.set(
          'hb_adid',
          Object.freeze([mutation === 'same_publisher_write' ? 'trusted' : 'publisher-new'])
        );
      }
      retireTargeting?.();

      expect(values.get('hb_adid')).toEqual([
        mutation === 'unchanged'
          ? 'publisher-original'
          : mutation === 'same_publisher_write'
            ? 'trusted'
            : 'publisher-new',
      ]);
      expect(setTargeting).toHaveBeenCalledTimes(mutation === 'unchanged' ? 1 : 0);
      expect(releaseObservation).toHaveBeenCalledOnce();
    }
  );

  it.each([
    { diagnosticsActive: false, adoptInitialDisplay: false, parserStateValid: true },
    { diagnosticsActive: true, adoptInitialDisplay: false, parserStateValid: true },
    { diagnosticsActive: false, adoptInitialDisplay: true, parserStateValid: true },
    { diagnosticsActive: false, adoptInitialDisplay: true, parserStateValid: false },
    { diagnosticsActive: false, adoptInitialDisplay: true, parserStateValid: 'absent' },
  ])(
    'uses only catalog capabilities without replaying adopted display (diagnostics=$diagnosticsActive, adoption=$adoptInitialDisplay, parser=$parserStateValid)',
    async ({ diagnosticsActive, adoptInitialDisplay, parserStateValid }) => {
      vi.useFakeTimers();
      const NativeMutationObserver = window.MutationObserver;
      const activeMutationObservers = new Set<MutationObserver>();
      class TrackingMutationObserver implements MutationObserver {
        readonly inner: MutationObserver;

        constructor(callback: MutationCallback) {
          this.inner = new NativeMutationObserver(callback);
        }

        disconnect(): void {
          activeMutationObservers.delete(this);
          this.inner.disconnect();
        }

        observe(target: Node, options?: MutationObserverInit): void {
          activeMutationObservers.add(this);
          this.inner.observe(target, options);
        }

        takeRecords(): MutationRecord[] {
          return this.inner.takeRecords();
        }
      }
      vi.stubGlobal('MutationObserver', TrackingMutationObserver);
      const listenerTypes: string[] = [];
      const removedTypes: string[] = [];
      const targeting = new Map<string, readonly string[]>();
      const publisherSlot = {
        clearTargeting: vi.fn((key?: string) => {
          if (key === undefined) targeting.clear();
          else targeting.delete(key);
          return publisherSlot;
        }),
        getAdUnitPath: () => '/123/spa-winner',
        getSlotElementId: () => 'spa-winner',
        getTargeting: (key: string) => targeting.get(key) ?? [],
        setTargeting: vi.fn((key: string, value: string | readonly string[]) => {
          targeting.set(key, typeof value === 'string' ? [value] : value);
          return publisherSlot;
        }),
      };
      const definedSlots: object[] = [];
      const createDefinedSlot = (adUnitPath: string, elementId: string) => ({
        addService: vi.fn(),
        clearTargeting: vi.fn(),
        getAdUnitPath: () => adUnitPath,
        getSlotElementId: () => elementId,
        getTargeting: () => [],
        setTargeting: vi.fn(),
      });
      const defineSlot = vi.fn((adUnitPath: string, _sizes: unknown, elementId: string) => {
        const slot = createDefinedSlot(adUnitPath, elementId);
        definedSlots.push(slot);
        return slot;
      });
      const destroySlots = vi.fn((slots: readonly object[]) => {
        for (const slot of slots) {
          const index = definedSlots.indexOf(slot);
          if (index >= 0) definedSlots.splice(index, 1);
        }
        return true;
      });
      const display = vi.fn();
      const refresh = vi.fn();
      const pubads = {
        addEventListener: vi.fn((type: string, _listener: (event: unknown) => void) => {
          listenerTypes.push(type);
        }),
        disableInitialLoad: vi.fn(),
        getSlots: vi.fn(() => [publisherSlot, ...definedSlots]),
        refresh,
        removeEventListener: vi.fn((type: string, _listener: (event: unknown) => void) => {
          removedTypes.push(type);
        }),
      };
      (window as Window & { googletag?: unknown }).googletag = {
        apiReady: true,
        pubadsReady: true,
        cmd: { push: (command: () => void) => (command(), 0) },
        defineSlot,
        destroySlots,
        display,
        getConfig: vi.fn(() => ({ disableInitialLoad: false })),
        pubads: () => pubads,
        setConfig: vi.fn(),
      };
      const providerFacades = new Map<string, Readonly<Record<string, unknown>>>();
      const protect = vi.fn(() => true);
      const bootManifest = Object.freeze({
        version: 1 as const,
        releaseId: RELEASE_ID,
        firstDisplay: null,
        runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`,
        integrations: Object.freeze([
          Object.freeze({ id: 'render_runtime', phase: 'takeover' as const }),
          Object.freeze({ id: 'gpt', phase: 'takeover' as const }),
        ]),
      });
      const runtime = Object.freeze({
        attachAuctionContextService: () => () => undefined,
        boot: () =>
          Object.freeze({
            auctionProjection: Object.freeze({
              version: 1,
              auction: Object.freeze({
                version: 1,
                auctionId: 'initial',
                results: Object.freeze([
                  Object.freeze({
                    slot: 'takeover-slot',
                    outcome: 'no_bid' as const,
                  }),
                ]),
              }),
              slots: Object.freeze([
                Object.freeze({
                  slot: 'takeover-slot',
                  gamUnitPath: '/123/takeover-slot',
                  divId: 'takeover-slot',
                  formats: Object.freeze([Object.freeze([300, 250] as const)]),
                  targeting: Object.freeze({}),
                }),
              ]),
              bids: Object.freeze([]),
            }),
            diagnostics: Object.freeze({
              version: 1,
              renderTraceOverlay: false,
              gpt: Object.freeze({ active: diagnosticsActive }),
            }),
            manifest: bootManifest,
          }),
        document,
        enqueue: () => true,
        generation: Object.freeze({}),
        protectFirstDisplayAttemptBatch: protect,
        registerAuctionContext: () => () => undefined,
      } satisfies RuntimeCapabilityV1);
      const registry = createIntegrationRegistry({
        manifest: bootManifest,
        releaseId: RELEASE_ID,
        knownIntegrationIds: Object.freeze(['render_runtime', 'gpt']),
        catalog: Object.freeze([
          Object.freeze({
            id: 'render_runtime',
            phase: 'takeover' as const,
            trigger: null,
            consumes: Object.freeze(['runtime.v1']),
            provides: Object.freeze([
              'slots.v1',
              'auction.v1',
              'render.v1',
              'messages.v1',
              'trace.v1',
              'trace.presentation.v1',
              'direct.v1',
            ]),
          }),
          Object.freeze({
            id: 'gpt',
            phase: 'takeover' as const,
            trigger: null,
            consumes: Object.freeze([
              'runtime.v1',
              'slots.v1',
              'auction.v1',
              'render.v1',
              'messages.v1',
              'trace.v1',
            ]),
            provides: Object.freeze(['gpt.v1', 'gpt.events.v1', 'pbs_cache.baseline.v1']),
          }),
        ]),
        runtimeCapability: runtime,
        getBindings: (id) =>
          Object.freeze({
            config: id === 'gpt' ? GPT_CONFIG : undefined,
            interfaces: Object.freeze({}),
          }),
        onCapabilityStaged: (key, facade) => {
          providerFacades.set(key, facade);
          return () => {
            if (providerFacades.get(key) === facade) providerFacades.delete(key);
          };
        },
        startedAtMs: 0,
        now: () => 0,
      });
      const takeoverElement = document.createElement('div');
      takeoverElement.id = 'takeover-slot';
      document.body.appendChild(takeoverElement);
      const adoptedFrame = document.createElement('iframe');
      takeoverElement.appendChild(adoptedFrame);
      expect(registry.register(createRenderRuntimeIntegrationRegistration(RELEASE_ID))).toBe(true);
      expect(registry.register(createProductionGptRegistration(RELEASE_ID))).toBe(true);

      const installCallbacks: IntegrationInstallCallbacks = {
        ...callbacks([]),
        ...(adoptInitialDisplay
          ? {
              coordinateTakeover: (
                prepared: Parameters<
                  NonNullable<IntegrationInstallCallbacks['coordinateTakeover']>
                >[0]
              ) => {
                const slices = Object.freeze(
                  parserStateValid === 'absent'
                    ? (['first_display'] as const)
                    : (['first_display', 'gpt_initial'] as const)
                );
                const handoff = prepared.validateHandoff(
                  Object.freeze({
                    version: 1 as const,
                    releaseId: RELEASE_ID,
                    generation: 1,
                    projectionDigest: 'b'.repeat(64),
                    integrationConfigDigest: 'c'.repeat(64),
                    slices,
                    slots: Object.freeze([
                      Object.freeze({
                        id: 'takeover-slot',
                        aliases: Object.freeze([]),
                        owner: 'publisher',
                        domId: 'takeover-slot',
                        gamPath: '/123/takeover-slot',
                        formats: Object.freeze([Object.freeze([300, 250])]),
                        outcome: 'accepted' as const,
                        targeting: Object.freeze([
                          Object.freeze(['hb_adid', RESERVATION_ID] as const),
                        ]),
                        committedArtifact: 'gpt_adm' as const,
                        targetingOwnership: Object.freeze([]),
                        gptToken: 'gt1_1',
                      }),
                    ]),
                    attempts: Object.freeze([
                      Object.freeze({
                        id: 'a1_AAECAwQFBgcAAAAAAAAAAQ',
                        slotId: 'takeover-slot',
                        ordinal: 1,
                        state: 'accepted' as const,
                        reason: null,
                      }),
                    ]),
                    tombstones: Object.freeze([]),
                    artifacts: Object.freeze([
                      Object.freeze({
                        hostPosition: null,
                        hostPositionPriority: null,
                        slotId: 'takeover-slot',
                        kind: 'gpt_adm' as const,
                        owner: 'trusted_server' as const,
                        token: RESERVATION_ID,
                      }),
                    ]),
                    parserState:
                      parserStateValid === true
                        ? Object.freeze([
                            Object.freeze({
                              sliceId: 'gpt_initial',
                              observations: Object.freeze(['protocol_version']),
                              values: Object.freeze([
                                Object.freeze(['protocol_version', 1] as const),
                              ]),
                            }),
                          ])
                        : Object.freeze([]),
                    gptDiagnostics: Object.freeze({
                      facts: Object.freeze([]),
                      overflowCount: 0,
                      dropCount: 0,
                    }),
                    timing: Object.freeze({
                      bidsScriptMs: 1,
                      firstDisplayMs: 2,
                      terminalMs: 3,
                      paintMs: 4,
                    }),
                    highWater: Object.freeze({
                      navigationAttemptPrefix: 'AAECAwQFBgc',
                      nextAttemptOrdinal: 2,
                      nextNavigationAttemptOrdinal: 2,
                      nextSlotRegistrationOrdinal: 2,
                      nextReservationOrdinal: 2,
                      nextTicketOrdinal: 1,
                      reservationClockEpochMs: 0,
                    }),
                    cycles: Object.freeze([
                      Object.freeze({
                        nextCycleOrdinal: 2,
                        quarantines: Object.freeze([]),
                        records: Object.freeze([
                          Object.freeze({
                            ordinal: 1,
                            responseIdentifier: 'response-one',
                            seen: Object.freeze(['slotRequested', 'slotRenderEnded'] as const),
                            state: 'completed' as const,
                          }),
                        ]),
                        slotId: 'takeover-slot',
                        token: 'gt1_1',
                        unknownPriorCycle: false,
                      }),
                    ]),
                    trace: Object.freeze({
                      nextGlobalSlotOrdinal: 2,
                      nextSequence: 2,
                      slots: Object.freeze([
                        Object.freeze({
                          bindings: Object.freeze([
                            Object.freeze({
                              atMs: 2,
                              cycleOrdinal: 1,
                              historySequence: 1,
                              state: 'completed' as const,
                              token: 'gt1_1',
                            }),
                          ]),
                          impressions: 1,
                          slotId: 'takeover-slot',
                        }),
                      ]),
                    }),
                    mutationRevision: 0,
                  }),
                  Object.freeze({
                    version: 1 as const,
                    releaseId: RELEASE_ID,
                    generation: 1,
                    projectionDigest: 'b'.repeat(64),
                    integrationConfigDigest: 'c'.repeat(64),
                    slices,
                    slotCount: 1,
                    outcomeCount: 1,
                    capabilities: Object.freeze([]),
                    objectKinds: Object.freeze(['gpt_slot', 'dom_artifact']),
                  })
                );
                if (!handoff) throw new Error('should validate test handoff');
                prepared.activate(
                  Object.freeze({
                    version: 1 as const,
                    adoptInitialDisplay: true as const,
                    handoff,
                    identities: Object.freeze([publisherSlot, adoptedFrame]),
                  })
                );
                prepared.commit();
              },
            }
          : {}),
      };
      const result = await registry.install(installCallbacks);
      if (adoptInitialDisplay && parserStateValid !== true) {
        expect(result).toMatchObject({ state: 'fallback', reason: 'bundle_partial' });
        expect(defineSlot).not.toHaveBeenCalled();
        expect(display).not.toHaveBeenCalled();
        expect(refresh).not.toHaveBeenCalled();
        vi.useRealTimers();
        return;
      }
      expect(result.state).toBe('kernel');
      const gpt = providerFacades.get('gpt.v1') as {
        activateLaterLifecycle: () => Readonly<{
          navigate: (path: string) => Promise<unknown>;
          release: () => void;
        }>;
        navigation: () => Readonly<{
          generation: object;
          currentAuctionProjection?: Readonly<{ auction?: Readonly<{ auctionId?: string }> }>;
        }>;
        slots: {
          request: (input: Readonly<Record<string, unknown>>) => unknown;
        };
      };
      if (adoptInitialDisplay) {
        await Promise.resolve();
        expect(defineSlot).not.toHaveBeenCalled();
        expect(display).not.toHaveBeenCalled();
        expect(refresh).not.toHaveBeenCalled();
        expect(protect).not.toHaveBeenCalled();
        if (result.state === 'kernel') result.dispose();
        expect(providerFacades.size).toBe(0);
        return;
      }
      await vi.waitFor(() => expect(definedSlots).toHaveLength(1));
      const takeoverNavigation = gpt.navigation();
      gpt.slots.request({
        intentId: 'takeover-request',
        navigationGeneration: takeoverNavigation.generation,
        operation: 'display',
        registeredSlotId: 'takeover-slot',
        requestClass: 'initial',
      });
      await vi.waitFor(() => expect(display).toHaveBeenCalledOnce());
      expect(protect).not.toHaveBeenCalled();
      expect(activeMutationObservers.size).toBe(3);
      const firstPhysicalSlot = definedSlots[0];
      expect(firstPhysicalSlot).toBeDefined();
      takeoverElement.remove();
      const replacementElement = document.createElement('div');
      replacementElement.id = 'takeover-slot';
      document.body.appendChild(replacementElement);
      await Promise.resolve();
      await vi.advanceTimersByTimeAsync(250);
      expect(destroySlots).toHaveBeenCalledOnce();
      const destroyedPhysicalSlot = destroySlots.mock.calls[0]?.[0]?.[0];
      expect(destroyedPhysicalSlot).toBeDefined();
      expect(destroyedPhysicalSlot).not.toBe(publisherSlot);
      expect(definedSlots).toHaveLength(1);
      expect(definedSlots).not.toContain(destroyedPhysicalSlot);
      expect(activeMutationObservers.size).toBe(3);
      expect(vi.getTimerCount()).toBe(0);
      vi.useRealTimers();
      expect([...providerFacades.keys()]).toEqual([
        'slots.v1',
        'auction.v1',
        'render.v1',
        'messages.v1',
        'trace.v1',
        'trace.presentation.v1',
        'direct.v1',
        'gpt.v1',
        'gpt.events.v1',
        'pbs_cache.baseline.v1',
      ]);
      expect(Reflect.ownKeys(providerFacades.get('pbs_cache.baseline.v1') ?? {})).toEqual([]);
      const render = providerFacades.get('render.v1') as {
        attachPucGamAttemptRegistrar: (registrar: (input: unknown) => boolean) => () => void;
      };
      expect(() => render.attachPucGamAttemptRegistrar(() => true)).toThrow('duplicated');
      expect(protect).not.toHaveBeenCalled();
      expect([...listenerTypes].sort()).toEqual(
        (diagnosticsActive
          ? [
              'slotRequested',
              'slotRenderEnded',
              'slotResponseReceived',
              'slotOnload',
              'impressionViewable',
              'slotVisibilityChanged',
            ]
          : ['slotRequested', 'slotRenderEnded']
        ).sort()
      );
      expect(listenerTypes.slice(0, 2)).toEqual(['slotRequested', 'slotRenderEnded']);

      const placement = {
        slot: 'spa-winner',
        gamUnitPath: '/123/spa-winner',
        divId: 'spa-winner',
        formats: [[300, 250]],
        targeting: {},
      };
      const bid = {
        candidateId: 'BBBBBBBBBBBB',
        slot: placement.slot,
        provider: 'trusted',
        upstreamBidId: 'spa-upstream',
        cpm: 2,
        currency: 'USD',
        targeting: { hb_bidder: 'trusted' },
        renderSource: {
          type: 'pbs_cache',
          version: 1,
          cacheId: 'cache id/with reserved bytes',
          cacheHost: 'cache.example:8443',
          cachePath: '/pbc/v1/cache',
          width: 300,
          height: 250,
        },
      };
      const pageBids = {
        version: 1,
        auction: {
          version: 1,
          auctionId: 'spa-production',
          results: [{ slot: placement.slot, outcome: 'winner', candidateId: bid.candidateId }],
        },
        slots: [placement],
        bids: [bid],
      };
      const slotElement = document.createElement('div');
      slotElement.id = placement.divId;
      document.body.appendChild(slotElement);
      const fetchPageBids = vi
        .spyOn(globalThis, 'fetch')
        .mockResolvedValue({ ok: true, json: async () => pageBids } as Response);
      const initialNavigation = gpt.navigation();
      expect(refresh).not.toHaveBeenCalled();
      const later = gpt.activateLaterLifecycle();
      expect(Object.isFrozen(later)).toBe(true);
      expect(activeMutationObservers.size).toBe(3);
      expect(() => gpt.activateLaterLifecycle()).toThrow('unavailable');
      const navigationResult = await later.navigate('/spa-production?route=one');
      expect(navigationResult).toEqual({
        status: 'committed',
        navigationGeneration: expect.any(Object),
        current: true,
      });
      expect(fetchPageBids).toHaveBeenCalledExactlyOnceWith(
        '/_ts/page-bids?path=%2Fspa-production%3Froute%3Done',
        expect.objectContaining({
          credentials: 'include',
          headers: { 'X-TSJS-Page-Bids': '1' },
          signal: expect.any(AbortSignal),
        })
      );
      expect(gpt.navigation()).not.toBe(initialNavigation);
      expect(gpt.navigation()?.currentAuctionProjection?.auction?.auctionId).toBe('spa-production');
      expect(refresh).toHaveBeenCalledExactlyOnceWith(
        [publisherSlot],
        Object.freeze({ changeCorrelator: false })
      );
      expect(publisherSlot.setTargeting).toHaveBeenCalledWith('hb_adid', bid.renderSource.cacheId);
      expect(publisherSlot.setTargeting).toHaveBeenCalledWith(
        'hb_cache_host',
        bid.renderSource.cacheHost
      );
      expect(publisherSlot.setTargeting).toHaveBeenCalledWith(
        'hb_cache_path',
        bid.renderSource.cachePath
      );

      const sourceFrame = document.createElement('iframe');
      slotElement.appendChild(sourceFrame);
      const responsePort = Object.freeze({ postMessage: vi.fn() });
      fetchPageBids.mockResolvedValueOnce({
        ok: true,
        status: 200,
        text: async () =>
          JSON.stringify({
            adm: '<main data-price="${AUCTION_PRICE}" data-b64="${AUCTION_PRICE:B64}">cached</main>',
            w: 320,
            h: 100,
            price: 2.75,
          }),
      } as Response);
      const requestEvent = new MessageEvent('message', {
        data: JSON.stringify({ message: 'Prebid Request', adId: bid.renderSource.cacheId }),
        ports: [responsePort as unknown as MessagePort],
        source: sourceFrame.contentWindow,
      });
      const stopImmediatePropagation = vi.spyOn(requestEvent, 'stopImmediatePropagation');
      window.dispatchEvent(requestEvent);
      await vi.waitFor(() => expect(responsePort.postMessage).toHaveBeenCalledOnce());
      expect(stopImmediatePropagation).toHaveBeenCalledOnce();
      expect(fetchPageBids).toHaveBeenNthCalledWith(
        2,
        'https://cache.example:8443/pbc/v1/cache?uuid=cache%20id%2Fwith%20reserved%20bytes',
        { mode: 'cors' }
      );
      expect(JSON.parse(String(responsePort.postMessage.mock.calls[0]?.[0]))).toEqual({
        message: 'Prebid Response',
        adId: bid.renderSource.cacheId,
        ad: '<main data-price="2.75" data-b64="${AUCTION_PRICE:B64}">cached</main>',
        renderer: expect.any(String),
        width: 320,
        height: 100,
      });

      let resolveStaleResponse!: (response: Response) => void;
      const concurrentPageBids = {
        version: 1,
        auction: {
          version: 1,
          auctionId: 'concurrent-current',
          results: [],
        },
        slots: [],
        bids: [],
      };
      fetchPageBids
        .mockReturnValueOnce(
          new Promise<Response>((resolve) => {
            resolveStaleResponse = resolve;
          })
        )
        .mockResolvedValueOnce({
          ok: true,
          json: async () => concurrentPageBids,
        } as Response);
      const staleNavigation = later.navigate('/stale-generation');
      await vi.waitFor(() => expect(fetchPageBids).toHaveBeenCalledTimes(3));
      const currentNavigation = later.navigate('/current-generation');
      await expect(currentNavigation).resolves.toEqual({
        status: 'committed',
        navigationGeneration: expect.any(Object),
        current: true,
      });
      resolveStaleResponse({ ok: true, json: async () => pageBids } as Response);
      const staleResult = await staleNavigation;
      expect(staleResult).toEqual({
        status: 'rejected',
        navigationGeneration: expect.any(Object),
        current: false,
      });
      expect((staleResult as { navigationGeneration: object }).navigationGeneration).not.toBe(
        ((await currentNavigation) as { navigationGeneration: object }).navigationGeneration
      );
      later.release();
      expect(activeMutationObservers.size).toBe(2);
      await later.navigate('/disposed-owner');
      expect(fetchPageBids).toHaveBeenCalledTimes(4);
      fetchPageBids.mockRestore();
      sourceFrame.remove();
      slotElement.remove();

      if (result.state === 'kernel') result.dispose();
      expect(activeMutationObservers.size).toBe(0);
      expect(removedTypes.sort()).toEqual([...listenerTypes].sort());
      expect(providerFacades.size).toBe(0);
      expect(() => render.attachPucGamAttemptRegistrar(() => true)).toThrow('unavailable');
    }
  );

  it('protects the complete immutable initial winner batch before starting either GPT request', async () => {
    const candidateIds = ['candidate001', 'candidate002'] as const;
    const projection = Object.freeze({
      version: 1 as const,
      auction: Object.freeze({
        version: 1 as const,
        auctionId: 'initial-winners',
        results: Object.freeze(
          candidateIds.map((candidateId, index) =>
            Object.freeze({
              slot: `slot-${index + 1}`,
              outcome: 'winner' as const,
              candidateId,
            })
          )
        ),
      }),
      slots: Object.freeze(
        candidateIds.map((_candidateId, index) =>
          Object.freeze({
            slot: `slot-${index + 1}`,
            gamUnitPath: `/123/slot-${index + 1}`,
            divId: `slot-${index + 1}`,
            formats: Object.freeze([Object.freeze([300, 250] as const)]),
            targeting: Object.freeze({}),
          })
        )
      ),
      bids: Object.freeze(
        candidateIds.map((candidateId, index) =>
          Object.freeze({
            candidateId,
            slot: `slot-${index + 1}`,
            provider: 'fictional',
            upstreamBidId: `upstream-${index + 1}`,
            cpm: index + 1,
            currency: 'USD' as const,
            targeting: Object.freeze({}),
            rendererReservationId: `r1_${String(index + 1).repeat(22)}`,
            renderSource: Object.freeze({
              type: 'adm' as const,
              version: 1 as const,
              adm: `<p>${index + 1}</p>`,
              width: 300,
              height: 250,
            }),
          })
        )
      ),
    });
    for (const placement of projection.slots) {
      const element = document.createElement('div');
      element.id = placement.divId;
      document.body.appendChild(element);
    }
    const runtime = createRuntimeSession({
      createIdentityIssuer: () =>
        createTestNavigationIdentityIssuer({
          getRandomValues: (target) => {
            target.fill(7);
            return target;
          },
        }),
    });
    const navigationResult = runtime.startInitialNavigation(projection);
    if (!navigationResult.ok) throw new Error(navigationResult.reason);
    const navigation = navigationResult.value;
    const artifacts = createCommittedArtifactStore();
    const reservations = createReservationService({
      prepareRenderSource: (candidate) =>
        typeof candidate === 'object' && candidate !== null && Object.isFrozen(candidate)
          ? (candidate as never)
          : undefined,
    });
    const physical = new Map<string, object>();
    const request = vi.fn(() =>
      Object.freeze({
        status: 'active' as const,
        result: Promise.resolve(
          Object.freeze({ status: 'failed' as const, reason: 'gpt_request_failed' as const })
        ),
        dispose: vi.fn(),
      })
    );
    const slots = Object.freeze({
      adoptGptSlot: (
        _generation: object,
        registeredSlotId: string,
        binding: Readonly<{ slot: object }>
      ) => {
        physical.set(registeredSlotId, binding.slot);
        return Object.freeze({ ok: true as const });
      },
      isBoundGptSlot: (_generation: object, registeredSlotId: string, slot: object) =>
        physical.get(registeredSlotId) === slot,
      recordPublisherDestruction: vi.fn(() => true),
      request,
    });
    const targeting = Object.freeze({
      observePublisherMutations: () =>
        Object.freeze({ status: 'completed', result: Promise.resolve(), dispose: vi.fn() }),
      own: (_slot: object, _key: string, _value: string, ownerId: string) =>
        Object.freeze({ ownerId, release: vi.fn() }),
    });
    const facade = Object.freeze({
      slots: () => Object.freeze([]),
      slotElementId: () => undefined,
      transactionalDefine: (
        definition: Readonly<{ elementId: string }>,
        _current: () => boolean,
        prepare: (slot: object) => Readonly<{ commit: () => boolean }>
      ) => {
        const slot = Object.freeze({ elementId: definition.elementId });
        if (!prepare(slot).commit()) return Object.freeze({ status: 'failed' as const });
        return Object.freeze({ status: 'defined' as const, slot });
      },
      clearTargeting: vi.fn(),
      getTargeting: vi.fn(() => Object.freeze([])),
      setTargeting: vi.fn(),
    });
    const googletag = Object.freeze({
      run: (command: (gpt: typeof facade) => unknown) =>
        Object.freeze({
          status: 'completed',
          result: Promise.resolve(command(facade)),
          dispose: vi.fn(),
        }),
    });
    let protectedLatches: readonly PromiseLike<unknown>[] | undefined;
    const protect = vi.fn((latches: readonly PromiseLike<unknown>[]) => {
      expect(Object.isFrozen(latches)).toBe(true);
      expect(latches).toHaveLength(2);
      expect(request).not.toHaveBeenCalled();
      protectedLatches = latches;
      return true;
    });

    await publishInitialGptProjection(document, {
      googletag: googletag as never,
      navigation,
      projection: projection as never,
      protect,
      pucBridge: Object.freeze({
        registerGamAttempt: vi.fn(() => true),
        recordNonemptyGam: vi.fn(() => true),
      }),
      render: Object.freeze({
        artifacts,
        createAttempt: (owner: Parameters<typeof createRenderAttempt>[0]['owner']) =>
          createRenderAttempt({
            artifacts,
            owner,
            prepareRenderSource: (candidate) => candidate as never,
            reservations,
          }),
        createSlotOperation,
        publisherOrigin: window.location.origin,
        registerRenderer: vi.fn(),
        rendererNonces: Object.freeze({}),
        renderWinner: vi.fn(() => false),
        reservations,
      }) as never,
      slots: slots as never,
      targeting: targeting as never,
    });

    expect(protect).toHaveBeenCalledOnce();
    expect(request).toHaveBeenCalledTimes(2);
    await Promise.allSettled([...(protectedLatches ?? [])]);
    artifacts.dispose();
    reservations.dispose();
    runtime.dispose();
  });

  it('prepares inertly, activates the reversible guard, and starts only after commit', async () => {
    const config = Object.freeze({ scriptUrl: '/integrations/gpt/script' });
    const order: string[] = [];
    const start = vi.fn((received: unknown) => {
      order.push('start');
      expect(received).toBe(config);
    });
    const release = vi.fn(() => order.push('release'));
    const activate = vi.fn(() => {
      order.push('gpt:activate');
      return release;
    });
    let finishPreparation: (() => void) | undefined;
    const preparationGate = new Promise<void>((resolve) => {
      finishPreparation = resolve;
    });
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'gate']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt', 'gate']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({ gpt: Object.freeze({ activate, start }) }),
      }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));
    registry.register(
      registration('gate', async () => {
        order.push('gate:prepare');
        await preparationGate;
        return Object.freeze({ activate: () => order.push('gate:activate') });
      })
    );
    const originalDocumentWrite = document.write;

    const installing = registry.install(callbacks(order));
    await vi.waitFor(() => expect(order).toEqual(['gate:prepare']));

    expect(isGuardInstalled()).toBe(false);
    expect(document.write).toBe(originalDocumentWrite);
    expect(start).not.toHaveBeenCalled();

    finishPreparation?.();
    const result = await installing;

    expect(result).toMatchObject({ state: 'kernel' });
    expect(isGuardInstalled()).toBe(true);
    expect(document.write).not.toBe(originalDocumentWrite);
    expect(order).toEqual([
      'gate:prepare',
      'core',
      'gpt:activate',
      'gate:activate',
      'publish',
      'start',
      'drain',
    ]);
    expect(activate).toHaveBeenCalledTimes(1);
    expect(start).toHaveBeenCalledExactlyOnceWith(config);

    if (result.state === 'kernel') {
      result.dispose();
      result.dispose();
    }
    expect(isGuardInstalled()).toBe(false);
    expect(document.write).toBe(originalDocumentWrite);
    expect(release).toHaveBeenCalledTimes(1);
  });

  it('unwinds the GPT guard before fallback when a later activation fails', async () => {
    const start = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'broken']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt', 'broken']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: GPT_CONFIG,
        interfaces: Object.freeze({
          gpt: Object.freeze({ activate: () => vi.fn(), start }),
        }),
      }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));
    registry.register(
      registration('broken', () => ({
        activate: () => {
          expect(isGuardInstalled()).toBe(true);
          throw new Error('fictional activation failure');
        },
      }))
    );

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });

    expect(isGuardInstalled()).toBe(false);
    expect(start).not.toHaveBeenCalled();
  });

  it('never installs the guard or starts when reversible GPT activation fails', async () => {
    const start = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: GPT_CONFIG,
        interfaces: Object.freeze({
          gpt: Object.freeze({
            activate: () => {
              expect(isGuardInstalled()).toBe(false);
              throw new Error('fictional observer activation failure');
            },
            start,
          }),
        }),
      }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(isGuardInstalled()).toBe(false);
    expect(start).not.toHaveBeenCalled();
  });

  it('fails preparation without effects when the composition omits the GPT boundary', async () => {
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({ config: GPT_CONFIG, interfaces: Object.freeze({}) }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });

    expect(isGuardInstalled()).toBe(false);
  });

  it.each([
    [
      'accessor',
      Object.freeze(
        Object.defineProperty({}, 'scriptUrl', {
          enumerable: true,
          get: () => '/publisher-controlled',
        })
      ),
    ],
    ['mutable nested data', Object.freeze({ nested: {} })],
    ['non-plain data', Object.freeze({ value: Object.freeze(new Date(0)) })],
  ])('rejects %s configuration during inert preparation', async (_caseName, config) => {
    const start = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({
          gpt: Object.freeze({ activate: () => vi.fn(), start }),
        }),
      }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(start).not.toHaveBeenCalled();
    expect(isGuardInstalled()).toBe(false);
  });

  it('isolates post-commit startup failure and disposes only the GPT module', async () => {
    const start = vi.fn(() => {
      throw new Error('fictional GPT startup failure');
    });
    const runtimeFailures: unknown[] = [];
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt']),
      startedAtMs: 0,
      now: () => 0,
      onRuntimeFailure: (failure) => runtimeFailures.push(failure),
      getBindings: () => ({
        config: GPT_CONFIG,
        interfaces: Object.freeze({
          gpt: Object.freeze({ activate: () => vi.fn(), start }),
        }),
      }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'kernel',
      runtimeFailures: [{ id: 'gpt', phase: 'after_commit' }],
    });

    expect(start).toHaveBeenCalledTimes(1);
    expect(runtimeFailures).toEqual([{ id: 'gpt', phase: 'after_commit' }]);
    expect(isGuardInstalled()).toBe(false);
  });

  it('starts fallback only after an attributable TS-owned empty cycle settles the primary', async () => {
    const harness = createAttemptHarness();
    const slot = deferredSlotOutcome();
    const order: string[] = [];
    let fallback: RenderAttempt | undefined;
    const bridgeInput: unknown[] = [];
    const bridge = {
      registerGamAttempt: vi.fn((input: GptSlotOperationInput) => {
        bridgeInput.push(input);
        return input.attempt.beginGamClaim();
      }),
      recordNonemptyGam: vi.fn(() => true),
    };
    const invokeCreateSlotOperation = vi.fn(createSlotOperation);
    const started = startGptSlotOperation({
      artifact: harness.artifact,
      attempt: harness.primary,
      createSlotOperation: invokeCreateSlotOperation,
      createFallback: (parentAttemptId) => {
        expect(harness.primary.snapshot().outcome).toEqual({
          outcome: 'failed',
          reason: 'gam_empty',
        });
        order.push('fallback:create');
        fallback = harness.createAttempt(parentAttemptId);
        return Object.freeze({ ok: true as const, value: fallback });
      },
      operation: 'refresh',
      owner: harness.primaryOwner,
      pucBridge: bridge,
      requestClass: 'primary',
      reservationId: RESERVATION_ID,
      slots: { request: slot.request },
    });

    expect(started.ok).toBe(true);
    expect(invokeCreateSlotOperation).toHaveBeenCalledExactlyOnceWith({
      primary: harness.primary,
      createFallback: expect.any(Function),
    });
    expect(bridge.registerGamAttempt).toHaveBeenCalledTimes(1);
    expect(slot.request).toHaveBeenCalledWith({
      intentId: harness.primary.id,
      navigationGeneration: harness.primary.navigationGeneration,
      operation: 'refresh',
      registeredSlotId: harness.primary.slot,
      requestClass: 'primary',
    });

    slot.resolve(Object.freeze({ status: 'empty', responseIdentifier: 'response-one' }));
    await Promise.resolve();

    expect(order).toEqual(['fallback:create']);
    expect(harness.primary.snapshot()).toMatchObject({
      state: 'failed',
      outcome: { outcome: 'failed', reason: 'gam_empty' },
    });
    expect(started.ok && started.value.snapshot()).toEqual({ settled: false });
    expect(slot.dispose).toHaveBeenCalledTimes(1);
    expect(bridge.recordNonemptyGam).not.toHaveBeenCalled();

    expect(fallback?.fail('gpt_request_failed')).toBe(true);
    expect(started.ok && started.value.snapshot()).toMatchObject({
      settled: true,
      result: {
        path: 'fallback',
        primaryAttemptId: harness.primary.id,
        primary: { outcome: 'failed', reason: 'gam_empty' },
        fallbackAttemptId: fallback?.id,
        fallback: { outcome: 'failed', reason: 'gpt_request_failed' },
      },
    });
    expect(harness.primary.snapshot().outcome).toEqual({
      outcome: 'failed',
      reason: 'gam_empty',
    });
    harness.runtime.dispose();
  });

  it('joins an attributable nonempty cycle to the PUC bridge without settling the operation', async () => {
    const harness = createAttemptHarness();
    const slot = deferredSlotOutcome();
    const registered: unknown[] = [];
    const nonempty: unknown[] = [];
    const bridge = {
      registerGamAttempt: vi.fn((input: GptSlotOperationInput) => {
        registered.push(input);
        return input.attempt.beginGamClaim();
      }),
      recordNonemptyGam: vi.fn((input: unknown) => {
        nonempty.push(input);
        return true;
      }),
    };
    const input = {
      artifact: harness.artifact,
      attempt: harness.primary,
      createSlotOperation,
      operation: 'display' as const,
      owner: harness.primaryOwner,
      pucBridge: bridge,
      requestClass: 'primary',
      reservationId: RESERVATION_ID,
      slots: { request: slot.request },
    };
    const started = startGptSlotOperation(input);
    slot.resolve(Object.freeze({ status: 'rendered', responseIdentifier: 'response-one' }));
    await Promise.resolve();

    expect(nonempty).toEqual(registered);
    expect(started.ok && started.value.snapshot()).toEqual({ settled: false });
    expect(harness.primary.snapshot().state).toBe('waiting_for_gam_and_claim');
    expect(slot.dispose).not.toHaveBeenCalled();

    harness.primary.cancel('superseded');
    expect(slot.dispose).toHaveBeenCalledTimes(1);
    harness.runtime.dispose();
  });

  it.each([
    [{ status: 'failed', reason: 'cycle_unattributable' }, 'cycle_unattributable'],
    [{ status: 'failed', reason: 'slot_quarantined' }, 'slot_quarantined'],
    [{ status: 'failed', reason: 'gpt_request_timeout' }, 'gpt_request_timeout'],
    [{ status: 'failed', reason: 'gpt_completion_timeout' }, 'gpt_completion_timeout'],
    [{ status: 'failed', reason: 'external_queue_full' }, 'external_queue_full'],
    [{ status: 'failed', reason: 'external_ready_timeout' }, 'external_ready_timeout'],
    [{ status: 'cancelled', reason: 'navigation_disposed' }, 'navigation_disposed'],
  ] as const)(
    'does not start fallback for non-empty terminal cycle outcome %s',
    async (slotOutcome, reason) => {
      const harness = createAttemptHarness();
      const slot = deferredSlotOutcome();
      const createFallback = vi.fn();
      const started = startGptSlotOperation({
        artifact: harness.artifact,
        attempt: harness.primary,
        createSlotOperation,
        createFallback,
        operation: 'refresh',
        owner: harness.primaryOwner,
        pucBridge: {
          registerGamAttempt: (input) => input.attempt.beginGamClaim(),
          recordNonemptyGam: () => true,
        },
        requestClass: 'primary',
        reservationId: RESERVATION_ID,
        slots: { request: slot.request },
      });
      slot.resolve(Object.freeze(slotOutcome) as SlotRequestOutcome);
      await Promise.resolve();

      expect(createFallback).not.toHaveBeenCalled();
      expect(started.ok && started.value.snapshot()).toMatchObject({
        settled: true,
        result: {
          path: 'primary',
          outcome: { reason },
        },
      });
      harness.runtime.dispose();
    }
  );
});

describe('ordered GPT winner publication', () => {
  function preparePublication() {
    const harness = createAttemptHarness();
    const source = Object.freeze({
      type: 'adm' as const,
      version: 1 as const,
      adm: '<main>trusted</main>',
      width: 300,
      height: 250,
    });
    const bid = Object.freeze({
      candidateId: 'AAAAAAAAAAAA',
      slot: harness.primary.slot,
      provider: 'trusted',
      upstreamBidId: 'upstream-one',
      cpm: 1.25,
      currency: 'USD' as const,
      targeting: Object.freeze({ hb_bidder: 'trusted' }),
      rendererReservationId: RESERVATION_ID,
      renderSource: source,
    });
    const placement = Object.freeze({
      slot: bid.slot,
      gamUnitPath: '/123/gpt-slot',
      divId: 'gpt-slot',
      formats: Object.freeze([Object.freeze([300, 250] as const)]),
      targeting: Object.freeze({ hb_bidder: 'publisher', pos: 'top' }),
    });
    const projection = Object.freeze({
      version: 1,
      auction: Object.freeze({
        version: 1,
        auctionId: 'gpt-publication',
        results: Object.freeze([
          Object.freeze({
            slot: bid.slot,
            outcome: 'winner' as const,
            candidateId: bid.candidateId,
          }),
        ]),
      }),
      slots: Object.freeze([placement]),
      bids: Object.freeze([bid]),
    });
    expect(harness.navigation.installAuctionProjection(projection)).toBe(true);

    const order: string[] = [];
    const values = new Map<string, readonly string[]>();
    const slot = Object.freeze({
      clearTargeting: vi.fn((key?: string) => {
        if (key === undefined) values.clear();
        else values.delete(key);
      }),
      getTargeting: vi.fn((key: string) => Object.freeze([...(values.get(key) ?? [])])),
      setTargeting: vi.fn((key: string, value: string | readonly string[]) => {
        order.push(`target:${key}`);
        values.set(key, Object.freeze(typeof value === 'string' ? [value] : [...value]));
      }),
    });
    const facade: GoogletagFacade = Object.freeze({
      bindingToken: () => Object.freeze({}),
      clearTargeting: (target: object, key?: string) => (target as typeof slot).clearTargeting(key),
      transactionalDefine: () => Object.freeze({ status: 'discarded' as const }),
      display: vi.fn(),
      getTargeting: (target: object, key: string) => (target as typeof slot).getTargeting(key),
      observeTargeting: () => {
        order.push('observe');
        return Object.assign(vi.fn(), { isCurrent: () => true });
      },
      refresh: vi.fn(),
      serviceState: () =>
        Object.freeze({ apiReady: true, initialLoadDisabled: false, pubadsReady: true }),
      setTargeting: (target: object, key: string, value: string | readonly string[]) =>
        (target as typeof slot).setTargeting(key, value),
      slotElementId: () => undefined,
      slots: () => Object.freeze([slot]),
      subscribe: () => vi.fn(),
      transactionalReplace: () => Object.freeze({ status: 'destroyed' as const }),
    });
    const googletag = Object.freeze({
      ...createNoopGoogletagAdapter(),
      bindingStatus: () => 'present' as const,
      run: <Value>(command: (gpt: Readonly<GoogletagFacade>) => Value) => {
        let result: Promise<Value>;
        try {
          result = Promise.resolve(command(facade));
        } catch (error) {
          result = Promise.reject(error);
        }
        return Object.freeze({ status: 'present' as const, result, dispose: vi.fn() });
      },
    });
    const targeting = createTargetingService();
    const slotOutcome = deferredSlotOutcome();
    const slots = {
      isBoundGptSlot: vi.fn(() => {
        order.push('slot:validate');
        return true;
      }),
      request: vi.fn((input: unknown) => {
        order.push('request');
        expect(input).toMatchObject({ registeredSlotId: bid.slot });
        expect(harness.reservations.recognize(RESERVATION_ID)).toMatchObject({
          recognized: true,
          state: 'renderable',
        });
        return slotOutcome.request();
      }),
    };
    let bridgeArtifact: CommittedRenderArtifact | undefined;
    const pucBridge = {
      registerGamAttempt: vi.fn((input: GptSlotOperationInput) => {
        order.push('bridge');
        bridgeArtifact = input.artifact;
        return input.attempt.beginGamClaim();
      }),
      recordNonemptyGam: vi.fn(() => true),
    };
    const reservations = {
      registerRender: vi.fn((input: Parameters<typeof harness.reservations.registerRender>[0]) => {
        order.push('reservation');
        return harness.reservations.registerRender(input);
      }),
      tombstone: harness.reservations.tombstone,
    };
    const input: GptWinnerPublicationInput = {
      artifact: harness.artifact,
      attempt: harness.primary,
      bid,
      createSlotOperation,
      googletag,
      navigation: harness.navigation,
      operation: 'refresh',
      owner: harness.primaryOwner,
      placement,
      pucBridge,
      requestClass: 'primary',
      reservations,
      slot,
      slots,
      targeting,
    };
    return {
      bid,
      bridgeArtifact: () => bridgeArtifact,
      harness,
      input,
      order,
      pucBridge,
      reservations,
      slot,
      slots,
      targeting,
      values,
    };
  }

  it('publishes reservation, targeting, intent, and request in that exact order', async () => {
    const publication = preparePublication();

    const result = await publishGptWinner(publication.input);

    expect(result.ok).toBe(true);
    expect(publication.order).toEqual([
      'slot:validate',
      'reservation',
      'observe',
      'slot:validate',
      'target:hb_adid',
      'target:hb_bidder',
      'target:pos',
      'slot:validate',
      'bridge',
      'request',
    ]);
    expect(publication.values).toEqual(
      new Map([
        ['hb_adid', [RESERVATION_ID]],
        ['hb_bidder', ['trusted']],
        ['pos', ['top']],
      ])
    );
    publication.bridgeArtifact()?.dispose();
    expect(publication.values.size).toBe(0);
    expect(publication.harness.reservations.recognize(RESERVATION_ID)).toMatchObject({
      recognized: true,
      state: 'disposed',
    });
    expect(publication.harness.artifact.dispose).toHaveBeenCalledTimes(1);
    publication.harness.runtime.dispose();
  });

  it('fails before targeting when exact slot ownership is lost across observation', async () => {
    const publication = preparePublication();
    publication.slots.isBoundGptSlot
      .mockImplementationOnce(() => {
        publication.order.push('slot:validate');
        return true;
      })
      .mockImplementation(() => {
        publication.order.push('slot:validate');
        return false;
      });

    await expect(publishGptWinner(publication.input)).resolves.toEqual({
      ok: false,
      reason: 'slot_unresolved',
    });
    expect(publication.order).toEqual(['slot:validate', 'reservation', 'observe', 'slot:validate']);
    expect(publication.values.size).toBe(0);
    expect(publication.slots.request).not.toHaveBeenCalled();
    expect(publication.harness.reservations.recognize(RESERVATION_ID)).toMatchObject({
      recognized: true,
      state: 'disposed',
    });
    expect(publication.harness.artifact.dispose).toHaveBeenCalledTimes(1);
    publication.harness.runtime.dispose();
  });

  it('compare-restores targeting when exact slot ownership is lost during writes', async () => {
    const publication = preparePublication();
    publication.slots.isBoundGptSlot
      .mockImplementationOnce(() => {
        publication.order.push('slot:validate');
        return true;
      })
      .mockImplementationOnce(() => {
        publication.order.push('slot:validate');
        return true;
      })
      .mockImplementation(() => {
        publication.order.push('slot:validate');
        return false;
      });

    await expect(publishGptWinner(publication.input)).resolves.toEqual({
      ok: false,
      reason: 'slot_unresolved',
    });
    expect(publication.order).toEqual([
      'slot:validate',
      'reservation',
      'observe',
      'slot:validate',
      'target:hb_adid',
      'target:hb_bidder',
      'target:pos',
      'slot:validate',
    ]);
    expect(publication.values.size).toBe(0);
    expect(publication.slots.request).not.toHaveBeenCalled();
    expect(publication.harness.reservations.recognize(RESERVATION_ID)).toMatchObject({
      recognized: true,
      state: 'disposed',
    });
    publication.harness.runtime.dispose();
  });

  it('rolls back targeting and tombstones when the bridge refuses before request', async () => {
    const publication = preparePublication();
    publication.pucBridge.registerGamAttempt.mockImplementation(() => {
      publication.order.push('bridge');
      return false;
    });

    await expect(publishGptWinner(publication.input)).resolves.toEqual({
      ok: false,
      reason: 'gpt_request_failed',
    });
    expect(publication.slots.request).not.toHaveBeenCalled();
    expect(publication.values.size).toBe(0);
    expect(publication.harness.reservations.recognize(RESERVATION_ID)).toMatchObject({
      recognized: true,
      state: 'disposed',
    });
    expect(publication.harness.artifact.dispose).toHaveBeenCalledTimes(1);
    publication.harness.runtime.dispose();
  });

  it('rolls back targeting and tombstones when the slot request throws', async () => {
    const publication = preparePublication();
    publication.slots.request.mockImplementation(() => {
      publication.order.push('request');
      throw new Error('fictional request failure');
    });

    await expect(publishGptWinner(publication.input)).resolves.toEqual({
      ok: false,
      reason: 'gpt_request_failed',
    });
    expect(publication.order).toEqual([
      'slot:validate',
      'reservation',
      'observe',
      'slot:validate',
      'target:hb_adid',
      'target:hb_bidder',
      'target:pos',
      'slot:validate',
      'bridge',
      'request',
    ]);
    expect(publication.values.size).toBe(0);
    expect(publication.harness.reservations.recognize(RESERVATION_ID)).toMatchObject({
      recognized: true,
      state: 'disposed',
    });
    expect(publication.harness.artifact.dispose).toHaveBeenCalledTimes(1);
    publication.harness.runtime.dispose();
  });

  it('fails before exposure when reservation insertion collides', async () => {
    const publication = preparePublication();
    expect(
      publication.harness.reservations.registerRender({
        reservationId: RESERVATION_ID,
        slot: publication.bid.slot,
        navigation: publication.harness.navigation,
        attemptId: publication.harness.primary.id,
        renderSource: publication.bid.renderSource,
        winnerContext: Object.freeze({ selectedCpm: publication.bid.cpm }),
      })
    ).toMatchObject({ ok: true });

    await expect(publishGptWinner(publication.input)).resolves.toEqual({
      ok: false,
      reason: 'reservation_collision',
    });
    expect(publication.order).toEqual(['slot:validate', 'reservation']);
    expect(publication.values.size).toBe(0);
    expect(publication.pucBridge.registerGamAttempt).not.toHaveBeenCalled();
    expect(publication.harness.artifact.dispose).toHaveBeenCalledTimes(1);
    publication.harness.runtime.dispose();
  });

  it('compare-restores earlier targeting when a later targeting write throws', async () => {
    const publication = preparePublication();
    publication.slot.setTargeting.mockImplementation((key, value) => {
      publication.order.push(`target:${key}`);
      if (key === 'hb_bidder') throw new Error('fictional targeting failure');
      publication.values.set(key, Object.freeze(typeof value === 'string' ? [value] : [...value]));
    });

    await expect(publishGptWinner(publication.input)).resolves.toEqual({
      ok: false,
      reason: 'gpt_request_failed',
    });
    expect(publication.values.size).toBe(0);
    expect(publication.slots.request).not.toHaveBeenCalled();
    expect(publication.harness.reservations.recognize(RESERVATION_ID)).toMatchObject({
      recognized: true,
      state: 'disposed',
    });
    publication.harness.runtime.dispose();
  });
});
