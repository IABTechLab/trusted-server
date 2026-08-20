import { afterEach, describe, expect, it, vi } from 'vitest';

import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  PreparedIntegration,
} from '../../../src/kernel/integration_registry';
import type {
  RuntimeAuctionContextService,
  RuntimeCapabilityV1,
} from '../../../src/kernel/runtime';
import {
  adoptInitialRenderArtifactsFromHandoff,
  adoptInitialRenderStateFromHandoff,
  createRenderRuntimeIntegrationRegistration,
} from '../../../src/integrations/render_runtime/module';
import { log } from '../../../src/core/log';
import {
  createArtifactHostPositionLeaseRegistry,
  createCommittedArtifactStore,
  type CommittedRenderArtifact,
  type RenderAttempt,
} from '../../../src/services/render';
import type { NavigationSession } from '../../../src/kernel/sessions';

const RELEASE_ID = 'a'.repeat(64);

afterEach(() => {
  document.body.replaceChildren();
});

describe('render_runtime provider', () => {
  it('adopts first-display replay tombstones and trace high-water state', () => {
    const adoptFirstDisplayIdentityState = vi.fn(() => true);
    const adoptTombstones = vi.fn(() => true);
    const adoptTrace = vi.fn(() => true);
    const navigationGeneration = {};
    const adoption = Object.freeze({
      version: 1 as const,
      adoptInitialDisplay: true as const,
      handoff: Object.freeze({
        highWater: Object.freeze({
          nextAttemptOrdinal: 7,
          nextNavigationAttemptOrdinal: 6,
          navigationAttemptPrefix: 'CAcGBQQDAgE',
          reservationClockEpochMs: 50,
        }),
        tombstones: Object.freeze([
          Object.freeze({
            expiresAtMs: 250,
            kind: 'reservation',
            ordinal: 4,
            value: `r1_${'a'.repeat(22)}`,
          }),
          Object.freeze({ expiresAtMs: 90, kind: 'ticket', ordinal: 2, value: 'ticket' }),
        ]),
        slots: Object.freeze([Object.freeze({ id: 'slot-1', domId: 'div-1' })]),
        cycles: Object.freeze([
          Object.freeze({
            records: Object.freeze([Object.freeze({ ordinal: 1, state: 'completed' as const })]),
            slotId: 'slot-1',
            token: 'gt1_1',
          }),
        ]),
        trace: Object.freeze({
          nextSequence: 9,
          slots: Object.freeze([
            Object.freeze({
              bindings: Object.freeze([
                Object.freeze({
                  atMs: 4,
                  cycleOrdinal: 1,
                  historySequence: 8,
                  state: 'completed' as const,
                  token: 'gt1_1',
                }),
              ]),
              impressions: 3,
              slotId: 'slot-1',
            }),
          ]),
        }),
      }),
      identities: Object.freeze([]),
    });

    expect(
      adoptInitialRenderStateFromHandoff(
        adoption,
        { adoptFirstDisplayIdentityState, generation: navigationGeneration },
        { adoptFirstDisplayTombstones: adoptTombstones },
        { adoptFirstDisplay: adoptTrace }
      )
    ).toBe(adoption);
    expect(adoptFirstDisplayIdentityState).toHaveBeenCalledExactlyOnceWith('CAcGBQQDAgE', 7);
    expect(adoptTrace).toHaveBeenCalledWith({
      navigationGeneration,
      nextSequence: 9,
      slots: [
        {
          bindings: [
            {
              cycleOrdinal: 1,
              historySequence: 8,
              state: 'completed',
              token: 'gt1_1',
            },
          ],
          impressions: 3,
          records: [
            {
              at: 4,
              count: 3,
              elementId: 'div-1',
              injected: true,
              path: 'ssat',
              rendered: true,
              seq: 8,
              servedFrom: 'inline',
              slotId: 'slot-1',
            },
          ],
          slotId: 'slot-1',
        },
      ],
    });
    expect(adoptTombstones).toHaveBeenCalledWith({
      clockEpochMs: 50,
      tombstones: [{ expiresAtMs: 250, reservationId: `r1_${'a'.repeat(22)}` }],
    });
  });

  it('adopts transferred DOM artifacts without removing them on rollback, then arms commit ownership', () => {
    const host = document.createElement('div');
    host.id = 'div-1';
    const frame = document.createElement('iframe');
    frame.srcdoc = '<!doctype html><title>Fictional creative</title>';
    host.append(frame);
    document.body.append(host);
    const physicalSlot = Object.freeze({});
    const adoption = Object.freeze({
      version: 1 as const,
      adoptInitialDisplay: true as const,
      handoff: Object.freeze({
        slots: Object.freeze([Object.freeze({ id: 'slot-1', domId: 'div-1' })]),
        cycles: Object.freeze([Object.freeze({ slotId: 'slot-1' })]),
        artifacts: Object.freeze([
          Object.freeze({
            hostPosition: null,
            hostPositionPriority: null,
            kind: 'gpt_adm' as const,
            owner: 'trusted_server' as const,
            slotId: 'slot-1',
            token: `r1_${'a'.repeat(22)}`,
          }),
        ]),
      }),
      identities: Object.freeze([physicalSlot, frame]),
    });
    const navigationGeneration = {};
    const batch = Object.freeze({
      createRenderAttempt: vi.fn(() =>
        Object.freeze({
          ok: true as const,
          value: Object.freeze({
            id: `a1_${'A'.repeat(22)}`,
            navigationGeneration,
          }),
        })
      ),
      dispose: vi.fn(),
    });
    const navigation = Object.freeze({
      generation: navigationGeneration,
      createAuctionBatch: vi.fn(() => batch),
      isCurrent: () => true,
    }) as unknown as NavigationSession;

    const rollbackStore = createCommittedArtifactStore();
    expect(
      adoptInitialRenderArtifactsFromHandoff(
        adoption,
        navigation,
        rollbackStore,
        createArtifactHostPositionLeaseRegistry(),
        document
      )
    ).toBeDefined();
    rollbackStore.dispose();
    expect(frame.isConnected).toBe(true);

    const committedStore = createCommittedArtifactStore();
    const committed = adoptInitialRenderArtifactsFromHandoff(
      adoption,
      navigation,
      committedStore,
      createArtifactHostPositionLeaseRegistry(),
      document
    );
    expect(committed?.adoption).toBe(adoption);
    committed?.arm();
    committedStore.dispose();
    expect(frame.isConnected).toBe(false);
    expect(batch.dispose).toHaveBeenCalledTimes(2);
  });

  it.each(['reparented', 'frame_style'] as const)(
    'retires an adopted APS mount on %s loss and compare-restores only owned style',
    (mutation) => {
      const host = document.createElement('div');
      host.id = 'div-1';
      host.style.setProperty('position', 'relative');
      const frame = document.createElement('iframe');
      frame.setAttribute('sandbox', 'allow-scripts');
      frame.src = 'https://example.com/renderer';
      host.append(frame);
      document.body.append(host);
      const physicalSlot = Object.freeze({});
      const navigationGeneration = {};
      const batch = Object.freeze({
        createRenderAttempt: vi.fn(() =>
          Object.freeze({
            ok: true as const,
            value: Object.freeze({ id: `a1_${'A'.repeat(22)}`, navigationGeneration }),
          })
        ),
        dispose: vi.fn(),
      });
      const navigation = Object.freeze({
        generation: navigationGeneration,
        createAuctionBatch: vi.fn(() => batch),
        isCurrent: () => true,
      }) as unknown as NavigationSession;
      const adoption = Object.freeze({
        version: 1 as const,
        adoptInitialDisplay: true as const,
        handoff: Object.freeze({
          slots: Object.freeze([Object.freeze({ id: 'slot-1', domId: 'div-1' })]),
          cycles: Object.freeze([Object.freeze({ slotId: 'slot-1' })]),
          artifacts: Object.freeze([
            Object.freeze({
              hostPosition: '',
              hostPositionPriority: '',
              kind: 'aps' as const,
              owner: 'trusted_server' as const,
              slotId: 'slot-1',
              token: `r1_${'a'.repeat(22)}`,
            }),
          ]),
        }),
        identities: Object.freeze([physicalSlot, frame]),
      });
      const store = createCommittedArtifactStore();
      const committed = adoptInitialRenderArtifactsFromHandoff(
        adoption,
        navigation,
        store,
        createArtifactHostPositionLeaseRegistry(),
        document
      );
      expect(committed).toBeDefined();
      committed?.arm();

      if (mutation === 'reparented') {
        const publisherContainer = document.createElement('div');
        document.body.append(publisherContainer);
        publisherContainer.append(frame);
      } else {
        frame.style.setProperty('visibility', 'hidden');
      }

      expect(store.sweep()).toBe(1);
      expect(frame.isConnected).toBe(false);
      expect(host.style.getPropertyValue('position')).toBe('');
      expect(store.sweep()).toBe(0);
    }
  );

  it('transfers an adopted APS host-position lease through persistent replacement', () => {
    const host = document.createElement('div');
    host.id = 'div-1';
    host.style.setProperty('position', 'relative');
    const frame = document.createElement('iframe');
    frame.src = 'https://example.com/renderer';
    host.append(frame);
    document.body.append(host);
    const physicalSlot = Object.freeze({});
    const navigationGeneration = {};
    const batch = Object.freeze({
      createRenderAttempt: vi.fn(() =>
        Object.freeze({
          ok: true as const,
          value: Object.freeze({ id: `a1_${'A'.repeat(22)}`, navigationGeneration }),
        })
      ),
      dispose: vi.fn(),
    });
    const navigation = Object.freeze({
      generation: navigationGeneration,
      createAuctionBatch: vi.fn(() => batch),
      isCurrent: () => true,
    }) as unknown as NavigationSession;
    const adoption = Object.freeze({
      version: 1 as const,
      adoptInitialDisplay: true as const,
      handoff: Object.freeze({
        slots: Object.freeze([Object.freeze({ id: 'slot-1', domId: 'div-1' })]),
        cycles: Object.freeze([Object.freeze({ slotId: 'slot-1' })]),
        artifacts: Object.freeze([
          Object.freeze({
            hostPosition: '',
            hostPositionPriority: '',
            kind: 'aps' as const,
            owner: 'trusted_server' as const,
            slotId: 'slot-1',
            token: `r1_${'a'.repeat(22)}`,
          }),
        ]),
      }),
      identities: Object.freeze([physicalSlot, frame]),
    });
    const store = createCommittedArtifactStore();
    const positions = createArtifactHostPositionLeaseRegistry();
    const committed = adoptInitialRenderArtifactsFromHandoff(
      adoption,
      navigation,
      store,
      positions,
      document
    );
    expect(committed).toBeDefined();
    committed?.arm();
    const predecessor = store.current('slot-1');
    if (!predecessor) throw new Error('should adopt the first-display APS artifact');
    let replacementDisposed = false;
    const replacement: CommittedRenderArtifact = Object.freeze({
      attemptId: `a1_${'B'.repeat(22)}`,
      kind: 'aps_mount' as const,
      navigationGeneration,
      slot: 'slot-1',
      dispose: () => {
        if (replacementDisposed) return;
        replacementDisposed = true;
        positions.release(replacement);
      },
    });

    expect(positions.inherit(replacement, predecessor, host)).toBe(true);
    expect(positions.claim(replacement)).toBe(true);
    expect(store.promote(replacement)).toBe(true);
    expect(frame.isConnected).toBe(false);
    expect(host.style.getPropertyValue('position')).toBe('relative');

    expect(store.release(replacement)).toBe(true);
    expect(host.style.getPropertyValue('position')).toBe('');
  });

  it('does not restore an adopted APS host style after a publisher replaces it', () => {
    const host = document.createElement('div');
    host.id = 'div-1';
    host.style.setProperty('position', 'relative');
    const frame = document.createElement('iframe');
    host.append(frame);
    document.body.append(host);
    const navigationGeneration = {};
    const batch = Object.freeze({
      createRenderAttempt: () =>
        Object.freeze({
          ok: true as const,
          value: Object.freeze({ id: `a1_${'A'.repeat(22)}`, navigationGeneration }),
        }),
      dispose: vi.fn(),
    });
    const navigation = Object.freeze({
      generation: navigationGeneration,
      createAuctionBatch: () => batch,
      isCurrent: () => true,
    }) as unknown as NavigationSession;
    const adoption = Object.freeze({
      version: 1 as const,
      adoptInitialDisplay: true as const,
      handoff: Object.freeze({
        slots: Object.freeze([Object.freeze({ id: 'slot-1', domId: 'div-1' })]),
        cycles: Object.freeze([Object.freeze({ slotId: 'slot-1' })]),
        artifacts: Object.freeze([
          Object.freeze({
            hostPosition: '',
            hostPositionPriority: '',
            kind: 'aps' as const,
            owner: 'trusted_server' as const,
            slotId: 'slot-1',
            token: `r1_${'a'.repeat(22)}`,
          }),
        ]),
      }),
      identities: Object.freeze([Object.freeze({}), frame]),
    });
    const store = createCommittedArtifactStore();
    const committed = adoptInitialRenderArtifactsFromHandoff(
      adoption,
      navigation,
      store,
      createArtifactHostPositionLeaseRegistry(),
      document
    );
    expect(committed).toBeDefined();
    committed?.arm();

    host.style.setProperty('position', 'absolute', 'important');
    expect(store.sweep()).toBe(1);
    expect(frame.isConnected).toBe(false);
    expect(host.style.getPropertyValue('position')).toBe('absolute');
    expect(host.style.getPropertyPriority('position')).toBe('important');
  });

  it('rolls back prepared resources without unbound disposer failures', () => {
    const release: Array<() => void> = [];
    const warn = vi.spyOn(log, 'warn').mockImplementation(() => undefined);
    const runtime = Object.freeze({
      attachAuctionContextService: () => () => undefined,
      boot: () =>
        Object.freeze({
          auctionProjection: Object.freeze({
            version: 1,
            auction: Object.freeze({
              version: 1,
              auctionId: 'initial',
              results: Object.freeze([]),
            }),
            slots: Object.freeze([]),
            bids: Object.freeze([]),
          }),
          diagnostics: Object.freeze({
            version: 1,
            renderTraceOverlay: false,
            gpt: Object.freeze({ active: false }),
          }),
          manifest: Object.freeze({
            version: 1,
            releaseId: RELEASE_ID,
            firstDisplay: null,
            runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${'b'.repeat(64)}`,
            integrations: Object.freeze([
              Object.freeze({ id: 'render_runtime', phase: 'takeover' as const }),
            ]),
          }),
        }),
      document,
      enqueue: () => true,
      generation: Object.freeze({}),
      protectFirstDisplayAttemptBatch: vi.fn(() => true),
      registerAuctionContext: () => () => undefined,
    } satisfies RuntimeCapabilityV1);

    createRenderRuntimeIntegrationRegistration(RELEASE_ID).prepare(
      Object.freeze({
        config: undefined,
        interfaces: Object.freeze({ 'runtime.v1': runtime }),
        signal: new AbortController().signal,
        onDispose: (callback: () => void) => release.push(callback),
      } satisfies IntegrationPrepareContext)
    );
    release.reverse().forEach((callback) => callback());

    expect(warn).not.toHaveBeenCalledWith('render_runtime disposal failed', expect.anything());
    warn.mockRestore();
  });

  it('stages the seven real capabilities inertly and activates direct registration once', async () => {
    const release: Array<() => void> = [];
    const activationRelease: Array<() => void> = [];
    const protect = vi.fn(() => true);
    let contextService: RuntimeAuctionContextService | undefined;
    const runtime = Object.freeze({
      attachAuctionContextService: (service: RuntimeAuctionContextService) => {
        if (contextService) return undefined;
        contextService = service;
        return () => {
          if (contextService === service) contextService = undefined;
        };
      },
      boot: () =>
        Object.freeze({
          auctionProjection: Object.freeze({
            version: 1,
            auction: Object.freeze({
              version: 1,
              auctionId: 'initial',
              results: Object.freeze([]),
            }),
            slots: Object.freeze([]),
            bids: Object.freeze([]),
          }),
          diagnostics: Object.freeze({
            version: 1,
            renderTraceOverlay: false,
            gpt: Object.freeze({ active: false }),
          }),
          manifest: Object.freeze({
            version: 1,
            releaseId: RELEASE_ID,
            firstDisplay: null,
            runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${'b'.repeat(64)}`,
            integrations: Object.freeze([
              Object.freeze({ id: 'render_runtime', phase: 'takeover' as const }),
              Object.freeze({ id: 'permutive_context', phase: 'takeover' as const }),
            ]),
          }),
        }),
      document,
      enqueue: () => true,
      generation: Object.freeze({}),
      protectFirstDisplayAttemptBatch: protect,
      registerAuctionContext: (
        integrationId: string,
        contributor: () => Readonly<Record<string, unknown>> | undefined
      ) => contextService?.register(integrationId, contributor),
    } satisfies RuntimeCapabilityV1);
    const registration = createRenderRuntimeIntegrationRegistration(RELEASE_ID);
    const prepared = registration.prepare(
      Object.freeze({
        config: undefined,
        interfaces: Object.freeze({ 'runtime.v1': runtime }),
        signal: new AbortController().signal,
        onDispose: (callback: () => void) => release.push(callback),
      } satisfies IntegrationPrepareContext)
    );
    if ('then' in Object(prepared)) throw new Error('render_runtime preparation must be sync');
    const exactPrepared = prepared as PreparedIntegration;
    const interfaces = exactPrepared.interfaces;
    expect(Reflect.ownKeys(interfaces ?? {})).toEqual([
      'slots.v1',
      'auction.v1',
      'render.v1',
      'messages.v1',
      'trace.v1',
      'trace.presentation.v1',
      'direct.v1',
    ]);
    const direct = interfaces?.['direct.v1'] as {
      addAdUnits: (candidate: unknown) => unknown;
      requestAds: (candidate?: unknown) => Promise<unknown>;
    };
    const slotCapability = interfaces?.['slots.v1'] as {
      attachPhysicalService: (service: object) => () => void;
      snapshot: () => readonly Readonly<{ registeredSlotId: string }>[];
    };
    expect(() =>
      direct.addAdUnits({
        code: 'programmatic',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
        bids: [{ bidder: 'fictional', params: {} }],
      })
    ).toThrow();

    exactPrepared.activate(
      Object.freeze({
        signal: new AbortController().signal,
        onDispose: (callback: () => void) => activationRelease.push(callback),
        afterCommit: vi.fn(),
      } satisfies IntegrationActivationContext)
    );
    expect(
      direct.addAdUnits({
        code: 'programmatic',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
        bids: [{ bidder: 'fictional', params: {} }],
      })
    ).toEqual({ registered: ['programmatic'] });
    const physicalRecords: Array<Readonly<Record<string, unknown>>> = [];
    const physicalService = Object.freeze({
      register: vi.fn(
        (_owner: object, registrations: readonly Readonly<Record<string, unknown>>[]) => {
          physicalRecords.push(
            ...registrations.map((registration) =>
              Object.freeze({
                ...registration,
                navigationGeneration: Object.freeze({}),
                domAliases: registration['domAliases'] ?? Object.freeze([]),
              })
            )
          );
          return Object.freeze({ ok: true as const, records: Object.freeze([...physicalRecords]) });
        }
      ),
      snapshotRegisteredSlots: vi.fn(() => Object.freeze([...physicalRecords])),
    });
    const releasePhysical = slotCapability.attachPhysicalService(physicalService);
    expect(physicalService.register).toHaveBeenCalledOnce();
    expect(slotCapability.snapshot().map(({ registeredSlotId }) => registeredSlotId)).toEqual([
      'programmatic',
    ]);
    releasePhysical();
    expect(slotCapability.snapshot().map(({ registeredSlotId }) => registeredSlotId)).toEqual([
      'programmatic',
    ]);
    const releaseContext = runtime.registerAuctionContext('permutive_context', () =>
      Object.freeze({ permutive_segments: Object.freeze(['segment-one']) })
    );
    expect(releaseContext).toBeTypeOf('function');
    const fetcher = vi.spyOn(globalThis, 'fetch').mockResolvedValue({
      ok: true,
      json: async () =>
        Object.freeze({
          id: 'auction-one',
          cur: 'USD',
          seatbid: Object.freeze([]),
          ext: Object.freeze({
            trusted_server: Object.freeze({
              slot_results: Object.freeze({
                version: 1,
                auctionId: 'auction-one',
                results: Object.freeze([
                  Object.freeze({ slot: 'programmatic', outcome: 'no_bid' as const }),
                ]),
              }),
            }),
          }),
        }),
    } as Response);
    await expect(direct.requestAds({ slots: ['programmatic'] })).resolves.toEqual({
      slots: [{ slot: 'programmatic', path: 'primary', outcome: 'no_bid' }],
    });
    expect(JSON.parse(String(fetcher.mock.calls[0]?.[1]?.body))).toMatchObject({
      config: { permutive_segments: ['segment-one'] },
    });
    fetcher.mockRestore();
    releaseContext?.();

    activationRelease.reverse().forEach((callback) => callback());
    release.reverse().forEach((callback) => callback());
    expect(() =>
      direct.addAdUnits({
        code: 'late',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
      })
    ).toThrow();
  });

  it('rejects renderer and APS-message registration until activation and removes exact registrations', () => {
    const release: Array<() => void> = [];
    const activationRelease: Array<() => void> = [];
    const runtime = Object.freeze({
      attachAuctionContextService: () => () => undefined,
      boot: () =>
        Object.freeze({
          auctionProjection: Object.freeze({
            version: 1,
            auction: Object.freeze({
              version: 1,
              auctionId: 'initial',
              results: Object.freeze([]),
            }),
            slots: Object.freeze([]),
            bids: Object.freeze([]),
          }),
          diagnostics: Object.freeze({
            version: 1,
            renderTraceOverlay: false,
            gpt: Object.freeze({ active: false }),
          }),
          manifest: Object.freeze({
            version: 1,
            releaseId: RELEASE_ID,
            firstDisplay: null,
            runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${'b'.repeat(64)}`,
            integrations: Object.freeze([
              Object.freeze({ id: 'render_runtime', phase: 'takeover' as const }),
            ]),
          }),
        }),
      document,
      enqueue: () => true,
      generation: Object.freeze({}),
      protectFirstDisplayAttemptBatch: vi.fn(() => true),
      registerAuctionContext: () => () => undefined,
    } satisfies RuntimeCapabilityV1);
    const prepared = createRenderRuntimeIntegrationRegistration(RELEASE_ID).prepare(
      Object.freeze({
        config: undefined,
        interfaces: Object.freeze({ 'runtime.v1': runtime }),
        signal: new AbortController().signal,
        onDispose: (callback: () => void) => release.push(callback),
      } satisfies IntegrationPrepareContext)
    ) as PreparedIntegration;
    const render = prepared.interfaces?.['render.v1'] as {
      attachPucGamAttemptRegistrar: (registrar: (input: unknown) => boolean) => () => void;
      createAttempt: (
        owner: Readonly<Record<string, unknown>>
      ) => Readonly<{ ok: boolean; value?: RenderAttempt }>;
      createSlotOperation: (
        input: Readonly<{ primary: RenderAttempt }>
      ) => Readonly<{ ok: true; value: object }> | Readonly<{ ok: false; reason: string }>;
      navigation: {
        createAuctionBatch: (auctionId: string) =>
          | {
              createRenderAttempt: (
                slot: string
              ) => Readonly<{ ok: boolean; value?: Readonly<Record<string, unknown>> }>;
            }
          | undefined;
      };
      registerPucGamAttempt: (input: unknown) => boolean;
      registerRenderer: (
        type: 'aps',
        renderer: (attempt: RenderAttempt, container: HTMLElement) => boolean
      ) => () => void;
    };
    const messages = prepared.interfaces?.['messages.v1'] as {
      messaging: {
        parseProtocolMessage: (kind: 'apsEnvelope', candidate: unknown) => object | undefined;
      };
      registerApsValidation: (validation: Readonly<Record<string, unknown>>) => () => void;
    };
    const renderer = vi.fn(() => true);
    const origin = window.location.origin;
    const rendererUrl = new URL('/integrations/aps/renderer/v2', origin).href;
    const validation = Object.freeze({
      expectedPublisherOrigin: origin,
      expectedRendererUrl: rendererUrl,
      validateApsRenderer: vi.fn(() => true),
    });
    const envelope = Object.freeze({
      version: 1,
      nonce: `n1_${'a'.repeat(22)}`,
      publisherOrigin: origin,
      renderer: Object.freeze({
        type: 'aps',
        version: 1,
        accountId: 'account',
        bidId: 'bid',
        tagType: 'iframe',
        creativeUrl: 'https://example.test/creative',
        width: 300,
        height: 250,
        aaxResponse: 'response',
      }),
    });

    expect(() => render.registerRenderer('aps', renderer)).toThrow('inactive');
    expect(() => render.attachPucGamAttemptRegistrar(() => true)).toThrow('unavailable');
    expect(render.registerPucGamAttempt(Object.freeze({}))).toBe(false);
    expect(() => messages.registerApsValidation(validation)).toThrow('inactive');
    expect(messages.messaging.parseProtocolMessage('apsEnvelope', envelope)).toBeUndefined();

    prepared.activate(
      Object.freeze({
        signal: new AbortController().signal,
        onDispose: (callback: () => void) => activationRelease.push(callback),
        afterCommit: vi.fn(),
      } satisfies IntegrationActivationContext)
    );
    const batch = render.navigation.createAuctionBatch('cross-bundle-render-capability');
    const owner = batch?.createRenderAttempt('slot-one');
    expect(owner?.ok).toBe(true);
    const attempt = render.createAttempt(owner?.value ?? Object.freeze({}));
    expect(attempt.ok).toBe(true);
    expect(render.createSlotOperation({ primary: attempt.value as RenderAttempt })).toMatchObject({
      ok: true,
    });
    const hostileCause = new Error('publisher-owned validation trap');
    const hostileValidation = new Proxy(Object.freeze({}), {
      getPrototypeOf: () => {
        throw hostileCause;
      },
    });
    let validationError: unknown;
    try {
      messages.registerApsValidation(hostileValidation);
    } catch (error) {
      validationError = error;
    }
    expect(validationError).toBeInstanceOf(TypeError);
    expect(validationError).toMatchObject({
      message: 'APS message validation is malformed',
      cause: hostileCause,
    });
    expect(Object.keys(validationError as object)).not.toContain('cause');
    const pucAttempt = Object.freeze({ marker: 'exact-attempt' });
    const pucRegistrar = vi.fn(() => true);
    const releasePucRegistrar = render.attachPucGamAttemptRegistrar(pucRegistrar);
    expect(render.registerPucGamAttempt(pucAttempt)).toBe(true);
    expect(pucRegistrar).toHaveBeenCalledExactlyOnceWith(pucAttempt);
    expect(() => render.attachPucGamAttemptRegistrar(() => true)).toThrow('duplicated');
    releasePucRegistrar();
    expect(render.registerPucGamAttempt(pucAttempt)).toBe(false);
    const releaseThrowingPucRegistrar = render.attachPucGamAttemptRegistrar(() => {
      throw new Error('contained GPT owner failure');
    });
    expect(render.registerPucGamAttempt(pucAttempt)).toBe(false);
    const releaseRenderer = render.registerRenderer('aps', renderer);
    const releaseValidation = messages.registerApsValidation(validation);
    expect(messages.messaging.parseProtocolMessage('apsEnvelope', envelope)).toEqual(envelope);
    expect(() => render.registerRenderer('aps', vi.fn())).toThrow('duplicated');
    expect(() => messages.registerApsValidation(validation)).toThrow('duplicated');

    releaseRenderer();
    releaseValidation();
    const replacementRenderer = vi.fn(() => false);
    const releaseReplacement = render.registerRenderer('aps', replacementRenderer);
    const releaseReplacementValidation = messages.registerApsValidation(validation);
    releaseRenderer();
    releaseValidation();
    expect(() => render.registerRenderer('aps', vi.fn())).toThrow('duplicated');
    expect(() => messages.registerApsValidation(validation)).toThrow('duplicated');

    activationRelease.reverse().forEach((callback) => callback());
    releaseThrowingPucRegistrar();
    expect(render.registerPucGamAttempt(pucAttempt)).toBe(false);
    expect(() => render.registerRenderer('aps', vi.fn())).toThrow('inactive');
    expect(() => messages.registerApsValidation(validation)).toThrow('inactive');
    expect(messages.messaging.parseProtocolMessage('apsEnvelope', envelope)).toBeUndefined();
    releaseReplacement();
    releaseReplacementValidation();
    release.reverse().forEach((callback) => callback());
  });

  it('publishes the data-only render trace through the private capability and public diagnostics', () => {
    const release: Array<() => void> = [];
    const activationRelease: Array<() => void> = [];
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
                Object.freeze({ slot: 'slot-one', outcome: 'no_bid' as const }),
              ]),
            }),
            slots: Object.freeze([
              Object.freeze({
                slot: 'slot-one',
                gamUnitPath: '/123/slot-one',
                divId: 'slot-one',
                formats: Object.freeze([Object.freeze([300, 250])]),
                targeting: Object.freeze({}),
              }),
            ]),
            bids: Object.freeze([]),
          }),
          diagnostics: Object.freeze({
            version: 1,
            renderTraceOverlay: true,
            gpt: Object.freeze({ active: false }),
          }),
          manifest: Object.freeze({
            version: 1,
            releaseId: RELEASE_ID,
            firstDisplay: null,
            runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${'b'.repeat(64)}`,
            integrations: Object.freeze([
              Object.freeze({ id: 'render_runtime', phase: 'takeover' as const }),
            ]),
          }),
        }),
      document,
      enqueue: () => true,
      generation: Object.freeze({}),
      protectFirstDisplayAttemptBatch: vi.fn(() => true),
      registerAuctionContext: () => () => undefined,
    } satisfies RuntimeCapabilityV1);
    const prepared = createRenderRuntimeIntegrationRegistration(RELEASE_ID).prepare(
      Object.freeze({
        config: undefined,
        interfaces: Object.freeze({ 'runtime.v1': runtime }),
        signal: new AbortController().signal,
        onDispose: (callback: () => void) => release.push(callback),
      } satisfies IntegrationPrepareContext)
    ) as PreparedIntegration;
    const trace = prepared.interfaces?.['trace.v1'] as {
      diagnostics: {
        current: () => Readonly<Record<string, Readonly<Record<string, unknown>>>>;
      };
      observations: { publish: (observation: Readonly<Record<string, unknown>>) => boolean };
      record: (record: Readonly<Record<string, unknown>>) => Readonly<Record<string, unknown>>;
    };
    const tracePresentation = prepared.interfaces?.['trace.presentation.v1'] as {
      attachPresentation: (factory: (source: object) => object) => () => void;
    };
    const direct = prepared.interfaces?.['direct.v1'] as {
      diagnostics: { renderTrace: object };
    };
    const slots = prepared.interfaces?.['slots.v1'] as {
      attachPhysicalService: (service: object) => () => void;
    };
    prepared.activate(
      Object.freeze({
        signal: new AbortController().signal,
        onDispose: (callback: () => void) => activationRelease.push(callback),
        afterCommit: vi.fn(),
      } satisfies IntegrationActivationContext)
    );
    let physicalRecords: readonly Readonly<Record<string, unknown>>[] = Object.freeze([]);
    const physicalService = Object.freeze({
      register: (
        owner: { generation: object },
        registrations: readonly Readonly<Record<string, unknown>>[]
      ) => {
        physicalRecords = Object.freeze(
          registrations.map((registration) =>
            Object.freeze({
              ...registration,
              domAliases: registration['domAliases'] ?? Object.freeze([]),
              navigationGeneration: owner.generation,
              traceToken: 'gt1_1',
            })
          )
        );
        return Object.freeze({ ok: true as const, records: physicalRecords });
      },
      resolveDomAlias: (alias: string) =>
        physicalRecords.find((record) =>
          (record['domAliases'] as readonly string[]).includes(alias)
        ),
      resolveRegisteredSlot: (slotId: string) =>
        physicalRecords.find((record) => record['registeredSlotId'] === slotId),
      snapshotRegisteredSlots: () => physicalRecords,
    });
    const releasePhysicalService = slots.attachPhysicalService(physicalService);

    expect(Reflect.ownKeys(trace)).toEqual([
      'record',
      'enrich',
      'prune',
      'diagnostics',
      'observations',
    ]);
    expect(Object.isFrozen(trace)).toBe(true);
    expect(Reflect.ownKeys(trace.observations)).toEqual(['publish']);
    expect('attachPresentation' in trace).toBe(false);
    expect(Reflect.ownKeys(tracePresentation)).toEqual(['attachPresentation']);
    expect(Object.isFrozen(tracePresentation)).toBe(true);
    expect(tracePresentation.attachPresentation).toBeTypeOf('function');
    expect(direct.diagnostics.renderTrace).toBe(trace.diagnostics);
    expect(document.getElementById('ts-render-trace-panel')).toBeNull();
    expect(
      trace.observations.publish(
        Object.freeze({
          kind: 'render_attempt',
          attemptId: 'attempt-one',
          slotId: 'slot-one',
          path: 'auction',
          rendered: true,
          injected: true,
          servedFrom: 'inline',
          state: 'accepted',
          outcome: Object.freeze({ outcome: 'accepted' }),
        })
      )
    ).toBe(true);
    expect(trace.diagnostics.current()['slot-one']).toMatchObject({
      slotId: 'slot-one',
      path: 'auction',
      rendered: true,
      injected: true,
      servedFrom: 'inline',
    });
    expect(
      trace.observations.publish(
        Object.freeze({
          kind: 'slotRequested',
          slot: Object.freeze({ token: 'gt1_1', cycleOrdinal: 1, elementId: 'slot-one' }),
        })
      )
    ).toBe(true);
    expect(
      trace.observations.publish(
        Object.freeze({
          kind: 'slotRenderEnded',
          slot: Object.freeze({ token: 'gt1_1', cycleOrdinal: 1, elementId: 'slot-one' }),
          isEmpty: false,
        })
      )
    ).toBe(true);
    expect(trace.diagnostics.current()['slot-one']).toMatchObject({
      count: 2,
      path: 'gam-refresh',
      rendered: true,
      servedFrom: 'gam',
    });
    expect(document.getElementById('ts-render-trace-panel')).toBeNull();

    activationRelease.reverse().forEach((callback) => callback());
    releasePhysicalService();
    release.reverse().forEach((callback) => callback());
    expect(trace.diagnostics.current()).toEqual({});
  });
});
