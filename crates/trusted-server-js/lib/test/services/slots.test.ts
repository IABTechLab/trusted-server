import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  createBrowserGoogletagAdapter,
  GoogletagReplacementError,
  type GoogletagAdapter,
  type GoogletagFacade,
  type GoogletagPublisherCallAdmission,
  type GoogletagReplacementCommitAdmission,
  type GoogletagReplacementDefinition,
} from '../../src/adapters/googletag';
import { createTestNavigationIdentityIssuer } from '../../src/kernel/identity';
import { createRuntimeSession, type NavigationSession } from '../../src/kernel/sessions';
import {
  MAX_ACTIVE_SLOT_RECORDS,
  createBrowserSlotReconciliationBoundary,
  createSlotService,
  type GptSlotBinding,
  type SlotReconciliationBoundary,
  type SlotRegistration,
  type SlotService,
} from '../../src/services/slots';

function createNavigation(): NavigationSession {
  return createRuntimeWithNavigation().navigation;
}

function createRuntimeWithNavigation() {
  const runtime = createRuntimeSession({
    createIdentityIssuer: () =>
      createTestNavigationIdentityIssuer({
        getRandomValues: (target) => {
          target.fill(1);
          return target;
        },
      }),
  });
  const result = runtime.startInitialNavigation();
  if (!result.ok) throw new Error('Expected a navigation');
  return { navigation: result.value, runtime };
}

function createGptHarness(
  options: {
    initialLoadDisabled?: boolean;
    deferDestroyedResult?: boolean;
    missingRefresh?: boolean;
    orphanOnReplace?: object;
    returnOldOnReplace?: boolean;
    synchronousRun?: boolean;
  } = {}
) {
  const listeners = new Map<string, Set<(event: unknown) => void>>();
  const slots: object[] = [];
  const display = vi.fn();
  const refresh = vi.fn();
  const destroySlots = vi.fn((_slots: readonly object[]) => true);
  const defineSlot = vi.fn(
    (_path: string, _sizes: unknown, elementId: string): object | undefined => {
      const slot = { elementId, replacement: true };
      slots.push(slot);
      return slot;
    }
  );
  const addService = vi.fn();
  const operationDisposals: Array<ReturnType<typeof vi.fn>> = [];
  const bindingToken = Object.freeze({});
  let deferredDestroyedResolved = false;
  let resolveDeferredDestroyedPromise!: () => void;
  const deferredDestroyedPromise = new Promise<void>((resolve) => {
    resolveDeferredDestroyedPromise = resolve;
  });
  let deferredDestroyedUsed = false;
  const facade: GoogletagFacade = Object.freeze({
    bindingToken: () => bindingToken,
    clearTargeting: vi.fn(),
    display,
    getTargeting: vi.fn(() => []),
    observeTargeting: () => Object.assign(vi.fn(), { isCurrent: () => true }),
    refresh: options.missingRefresh
      ? (undefined as unknown as GoogletagFacade['refresh'])
      : refresh,
    serviceState: () =>
      Object.freeze({
        apiReady: true,
        initialLoadDisabled: options.initialLoadDisabled === true,
        pubadsReady: true,
      }),
    setTargeting: vi.fn(),
    slots: () => Object.freeze([...slots]),
    subscribe: (eventType: string, listener: (event: unknown) => void) => {
      const registered = listeners.get(eventType) ?? new Set();
      registered.add(listener);
      listeners.set(eventType, registered);
      return () => registered.delete(listener);
    },
    transactionalReplace: (
      oldSlot: object,
      definition: GoogletagReplacementDefinition | undefined,
      isCurrent: () => boolean,
      prepareCommit: (replacement: object) => GoogletagReplacementCommitAdmission
    ) => {
      if (!destroySlots([oldSlot])) throw new Error('gpt_request_failed');
      if (!definition || !isCurrent()) return Object.freeze({ status: 'destroyed' as const });
      const replacement = options.returnOldOnReplace
        ? oldSlot
        : defineSlot(definition.adUnitPath, definition.sizes, definition.elementId);
      if (!replacement) throw new GoogletagReplacementError(undefined, true);
      if (replacement === oldSlot) {
        if (!destroySlots([replacement])) {
          throw new GoogletagReplacementError(replacement, true);
        }
        throw new GoogletagReplacementError(undefined, true);
      }
      if (!isCurrent()) {
        destroySlots([replacement]);
        return Object.freeze({ status: 'destroyed' as const });
      }
      addService(replacement);
      if (options.orphanOnReplace) {
        throw new GoogletagReplacementError(options.orphanOnReplace, true);
      }
      if (!isCurrent()) {
        destroySlots([replacement]);
        return Object.freeze({ status: 'destroyed' as const });
      }
      const admission = prepareCommit(replacement);
      if (!admission.commit()) {
        admission.rollback();
        destroySlots([replacement]);
        throw new Error('gpt_request_failed');
      }
      if (!isCurrent()) {
        admission.rollback();
        destroySlots([replacement]);
        return Object.freeze({ status: 'destroyed' as const });
      }
      return Object.freeze({ status: 'replaced' as const, slot: replacement });
    },
  });
  const adapter: GoogletagAdapter = Object.freeze({
    bindingStatus: () => 'present',
    dispose: vi.fn(),
    notifyReady: vi.fn(),
    observePublisherCalls: () => vi.fn(),
    run: <T>(command: (gpt: Readonly<GoogletagFacade>) => T) => {
      let disposed = false;
      const dispose = vi.fn(() => {
        disposed = true;
      });
      operationDisposals.push(dispose);
      let result: Promise<T>;
      if (options.synchronousRun !== false) {
        try {
          const value = command(facade);
          const deferResult =
            options.deferDestroyedResult === true &&
            !deferredDestroyedUsed &&
            typeof value === 'object' &&
            value !== null &&
            'status' in value &&
            value.status === 'destroyed';
          if (deferResult) {
            deferredDestroyedUsed = true;
            result = deferredDestroyedPromise.then(() => value);
          } else {
            result = Promise.resolve(value);
          }
        } catch (error) {
          result = Promise.reject(error);
        }
      } else {
        result = Promise.resolve().then(() => {
          if (disposed) throw new Error('disposed');
          return command(facade);
        });
      }
      return Object.freeze({
        status: 'present' as const,
        result,
        dispose,
      });
    },
  });
  return {
    adapter,
    addService,
    defineSlot,
    destroySlots,
    display,
    emit: (type: string, event: unknown) => {
      for (const listener of listeners.get(type) ?? []) listener(event);
    },
    facade,
    operationDisposals,
    refresh,
    resolveDeferredDestroyed: () => {
      if (deferredDestroyedResolved) return;
      deferredDestroyedResolved = true;
      resolveDeferredDestroyedPromise();
    },
  };
}

function serverRegistration(
  id: string,
  overrides: Partial<SlotRegistration> = {}
): SlotRegistration {
  return {
    registeredSlotId: id,
    source: 'server',
    ...overrides,
  };
}

function bindTrustedSlot(service: SlotService, navigation: NavigationSession, id = 'slot') {
  const slot = { id };
  expect(
    service.register(navigation, [
      serverRegistration(id, {
        adUnitCode: `/network/${id}`,
        domAliases: [`${id}-div`],
      }),
    ])
  ).toMatchObject({ ok: true });
  expect(
    service.adoptGptSlot(navigation.generation, id, {
      definition: {
        adUnitPath: `/network/${id}`,
        elementId: `${id}-div`,
        sizes: Object.freeze([[300, 250]]),
      },
      ownership: 'trusted_server',
      slot,
    })
  ).toEqual({ ok: true });
  return slot;
}

function createReconciliationBoundary() {
  let listener: (() => void) | undefined;
  const connected = new WeakSet<object>();
  const elements = new Map<string, object[]>();
  const observe = vi.fn((callback: () => void) => {
    listener = callback;
    return vi.fn(() => {
      if (listener === callback) listener = undefined;
    });
  });
  const boundary: SlotReconciliationBoundary = Object.freeze({
    observe,
    isConnected: (element: object) => connected.has(element),
    resolve: (elementIds: readonly string[]) => {
      const matches = new Set<object>();
      let matchedId: string | undefined;
      for (const elementId of elementIds) {
        for (const element of elements.get(elementId) ?? []) {
          if (!connected.has(element)) continue;
          matches.add(element);
          matchedId = elementId;
        }
      }
      if (matches.size === 0) return Object.freeze({ status: 'unresolved' as const });
      if (matches.size !== 1 || matchedId === undefined) {
        return Object.freeze({ status: 'ambiguous' as const });
      }
      return Object.freeze({
        status: 'unique' as const,
        element: [...matches][0]!,
        elementId: matchedId,
      });
    },
  });
  const put = (elementId: string, element: object): void => {
    connected.add(element);
    elements.set(elementId, [element]);
  };
  const replace = (elementId: string, element: object): void => {
    const previous = elements.get(elementId) ?? [];
    for (const candidate of previous) connected.delete(candidate);
    put(elementId, element);
    listener?.();
  };
  const replaceAmbiguously = (elementId: string, replacements: readonly object[]): void => {
    const previous = elements.get(elementId) ?? [];
    for (const candidate of previous) connected.delete(candidate);
    for (const replacement of replacements) connected.add(replacement);
    elements.set(elementId, [...replacements]);
    listener?.();
  };
  const disconnect = (elementId: string): void => {
    for (const candidate of elements.get(elementId) ?? []) connected.delete(candidate);
    elements.delete(elementId);
    listener?.();
  };
  return {
    boundary,
    disconnect,
    observe,
    put,
    replace,
    replaceAmbiguously,
    trigger: () => listener?.(),
  };
}

describe('slot registry', () => {
  afterEach(() => vi.useRealTimers());

  it('accepts exact nonempty 256-byte ids and rejects empty, 257-byte, and ASCII controls', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    const valid = `${'a'.repeat(254)}é`;

    expect(new TextEncoder().encode(valid)).toHaveLength(256);
    expect(service.register(navigation, [serverRegistration(valid)])).toMatchObject({ ok: true });

    for (const invalid of [
      '',
      'a'.repeat(257),
      'nul\0id',
      'line\nid',
      `del${String.fromCharCode(0x7f)}id`,
    ]) {
      expect(service.register(navigation, [serverRegistration(invalid)])).toEqual({
        ok: false,
        reason: 'invalid_slot_id',
      });
    }

    expect(
      service.register(navigation, [serverRegistration(`c1${String.fromCharCode(0x85)}id`)])
    ).toMatchObject({ ok: true });
  });

  it('reserves the combined 256-record capacity atomically', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    const first = Array.from({ length: 255 }, (_, index) => serverRegistration(`server-${index}`));

    expect(service.register(navigation, first)).toMatchObject({ ok: true });
    expect(
      service.register(navigation, [
        { registeredSlotId: 'programmatic-256', source: 'programmatic' },
      ])
    ).toMatchObject({ ok: true });
    expect(service.snapshotForTest().records).toBe(MAX_ACTIVE_SLOT_RECORDS);
    expect(
      service.register(navigation, [
        { registeredSlotId: 'programmatic-257', source: 'programmatic' },
      ])
    ).toEqual({ ok: false, reason: 'registry_capacity' });
    expect(service.resolveRegisteredSlot('programmatic-257')).toBeUndefined();
    expect(service.snapshotForTest().records).toBe(MAX_ACTIVE_SLOT_RECORDS);
  });

  it('snapshots navigation-local registration order with detached programmatic auction units', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const { navigation, runtime } = createRuntimeWithNavigation();
    const directAuctionUnit = Object.freeze({ code: 'programmatic' });

    expect(
      service.register(navigation, [
        serverRegistration('server'),
        {
          directAuctionUnit,
          registeredSlotId: 'programmatic',
          source: 'programmatic',
        },
      ])
    ).toMatchObject({ ok: true });
    expect(service.snapshotRegisteredSlots(navigation)).toEqual([
      expect.objectContaining({ ordinal: 0, registeredSlotId: 'server', source: 'server' }),
      expect.objectContaining({
        directAuctionUnit,
        ordinal: 1,
        registeredSlotId: 'programmatic',
        source: 'programmatic',
      }),
    ]);
    expect(Object.isFrozen(service.snapshotRegisteredSlots(navigation))).toBe(true);

    expect(
      service.register(navigation, [
        {
          directAuctionUnit: { code: 'unfrozen' },
          registeredSlotId: 'unfrozen',
          source: 'programmatic',
        },
      ])
    ).toEqual({ ok: false, reason: 'invalid_slot_id' });

    runtime.dispose();
    expect(service.snapshotRegisteredSlots(navigation)).toBeUndefined();
  });

  it('rejects exact registered-id collisions without partial indexes', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    expect(service.register(navigation, [serverRegistration('existing')])).toMatchObject({
      ok: true,
    });

    expect(
      service.register(navigation, [
        serverRegistration('fresh', { domAliases: ['fresh-div'] }),
        serverRegistration('existing', { domAliases: ['leaked-div'] }),
      ])
    ).toEqual({ ok: false, reason: 'duplicate_slot' });
    expect(service.resolveRegisteredSlot('fresh')).toBeUndefined();
    expect(service.resolveDomAlias('fresh-div')).toBeUndefined();
  });

  it('resolves only unique ad-unit codes and DOM aliases without normalizing or choosing first', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    expect(
      service.register(navigation, [
        serverRegistration('Exact-Slot', { adUnitCode: '/same', domAliases: ['same-div'] }),
        serverRegistration('other', { adUnitCode: '/same', domAliases: ['same-div'] }),
      ])
    ).toMatchObject({ ok: true });

    expect(service.resolveRegisteredSlot('Exact-Slot')?.registeredSlotId).toBe('Exact-Slot');
    expect(service.resolveRegisteredSlot('exact-slot')).toBeUndefined();
    expect(service.resolveAdUnitCode('/same')).toBeUndefined();
    expect(service.resolveDomAlias('same-div')).toBeUndefined();
  });

  it('binds one GPT object identity to at most one record and releases navigation records', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    const shared = {};
    expect(
      service.register(navigation, [serverRegistration('one'), serverRegistration('two')])
    ).toMatchObject({ ok: true });
    expect(
      service.adoptGptSlot(navigation.generation, 'one', {
        ownership: 'publisher',
        slot: shared,
      })
    ).toEqual({ ok: true });
    expect(
      service.adoptGptSlot(navigation.generation, 'two', {
        ownership: 'publisher',
        slot: shared,
      })
    ).toEqual({ ok: false, reason: 'gpt_object_collision' });

    navigation.dispose();
    expect(service.snapshotForTest().records).toBe(0);
    expect(service.resolveRegisteredSlot('one')).toBeUndefined();
  });

  it('latches a publication request to the exact bound GPT identity', async () => {
    const gpt = createGptHarness();
    const service = createSlotService({ googletag: gpt.adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    service.activate();

    const stale = service.request({
      expectedSlot: {},
      intentId: 'stale-publication',
      navigationGeneration: navigation.generation,
      operation: 'display',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await expect(stale.result).resolves.toEqual({ status: 'failed', reason: 'slot_unresolved' });
    expect(gpt.display).not.toHaveBeenCalled();

    const current = service.request({
      expectedSlot: slot,
      intentId: 'current-publication',
      navigationGeneration: navigation.generation,
      operation: 'display',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    await Promise.resolve();
    expect(current.status).toBe('active');
    expect(gpt.display).toHaveBeenCalledExactlyOnceWith(slot);
  });

  it('recognizes the exact live GPT binding regardless of who defined the slot', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const { navigation, runtime } = createRuntimeWithNavigation();
    const trustedSlot = bindTrustedSlot(service, navigation, 'trusted');

    expect(service.isBoundGptSlot(navigation.generation, 'trusted', trustedSlot)).toBe(true);
    expect(service.isBoundGptSlot(navigation.generation, 'other', trustedSlot)).toBe(false);
    expect(service.isBoundGptSlot({}, 'trusted', trustedSlot)).toBe(false);
    expect(service.isBoundGptSlot(navigation.generation, 'trusted', {})).toBe(false);

    const publisherSlot = {};
    expect(service.register(navigation, [serverRegistration('publisher')])).toMatchObject({
      ok: true,
    });
    expect(
      service.adoptGptSlot(navigation.generation, 'publisher', {
        ownership: 'publisher',
        slot: publisherSlot,
      })
    ).toEqual({ ok: true });
    expect(service.isBoundGptSlot(navigation.generation, 'publisher', publisherSlot)).toBe(true);

    runtime.dispose();
    expect(service.isBoundGptSlot(navigation.generation, 'trusted', trustedSlot)).toBe(false);
  });

  it('hands an exact late publisher definition the TS slot and consumes only duplicate requests', async () => {
    const gpt = createGptHarness({ initialLoadDisabled: true });
    const warnPublisherHandoffMismatch = vi.fn(() => {
      throw new Error('fictional local logger failure');
    });
    const service = createSlotService({
      googletag: gpt.adapter,
      warnPublisherHandoffMismatch,
    });
    const { navigation, runtime } = createRuntimeWithNavigation();
    const slot = bindTrustedSlot(service, navigation);

    expect(
      service.claimPublisherGptSlot({
        adUnitPath: '/publisher/mismatch',
        elementId: 'slot-div',
        initialLoadDisabled: true,
        sizes: Object.freeze([[728, 90]]),
      })
    ).toEqual({ action: 'handoff', slot });
    expect(warnPublisherHandoffMismatch).toHaveBeenCalledExactlyOnceWith(
      'GPT publisher handoff metadata mismatch',
      Object.freeze({ formatsMismatch: true, pathMismatch: true })
    );
    expect(JSON.stringify(warnPublisherHandoffMismatch.mock.calls[0]).length).toBeLessThanOrEqual(
      128
    );
    expect(
      service.preparePublisherDisplay({ initialLoadDisabled: true, target: 'slot-div' })
    ).toEqual({ action: 'suppress' });
    expect(
      service.preparePublisherDisplay({ initialLoadDisabled: true, target: 'slot-div' })
    ).toEqual({ action: 'forward' });

    const unrelated = {};
    expect(
      service.preparePublisherRefresh({
        requestedSlots: undefined,
        slots: Object.freeze([slot, unrelated]),
      })
    ).toEqual({ action: 'replace', slots: [unrelated] });
    const forwardedRefresh = service.preparePublisherRefresh({
      requestedSlots: Object.freeze([slot]),
      slots: Object.freeze([slot]),
    });
    expect(forwardedRefresh.action).toBe('forward');
    if (forwardedRefresh.action === 'forward') {
      expect(forwardedRefresh.admission).toBeDefined();
      forwardedRefresh.admission?.commit();
    }

    const request = service.request({
      intentId: 'after-publisher-refresh',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await expect(request.result).resolves.toEqual({
      status: 'failed',
      reason: 'cycle_unattributable',
    });

    runtime.dispose();
    expect(gpt.destroySlots).not.toHaveBeenCalled();
  });

  it('does not warn when an exact publisher handoff matches path and formats', () => {
    const warnPublisherHandoffMismatch = vi.fn();
    const service = createSlotService({
      googletag: createGptHarness().adapter,
      warnPublisherHandoffMismatch,
    });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);

    expect(
      service.claimPublisherGptSlot({
        adUnitPath: '/network/slot',
        elementId: 'slot-div',
        initialLoadDisabled: false,
        sizes: Object.freeze([[300, 250]]),
      })
    ).toEqual({ action: 'handoff', slot });
    expect(warnPublisherHandoffMismatch).not.toHaveBeenCalled();
  });

  it('rolls back a pending publisher display without settling active or queued TS work', async () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    expect(
      service.claimPublisherGptSlot({
        adUnitPath: '/network/slot',
        elementId: 'slot-div',
        initialLoadDisabled: false,
        sizes: [300, 250],
      })
    ).toEqual({ action: 'handoff', slot });
    expect(
      service.preparePublisherDisplay({ initialLoadDisabled: false, target: 'slot-div' })
    ).toEqual({ action: 'suppress' });
    const active = service.request({
      intentId: 'active-before-publisher-display',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    const queued = service.request({
      intentId: 'queued-before-publisher-display',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    expect(active.status).toBe('active');
    expect(queued.status).toBe('queued');

    const decision = service.preparePublisherDisplay({
      initialLoadDisabled: false,
      target: 'slot-div',
    }) as Readonly<{ action: 'forward'; admission?: GoogletagPublisherCallAdmission }>;
    expect(decision.action).toBe('forward');
    expect(decision.admission).toBeDefined();
    expect(active.status).toBe('active');
    expect(queued.status).toBe('queued');

    decision.admission?.rollback();
    decision.admission?.rollback();

    expect(active.status).toBe('active');
    expect(queued.status).toBe('queued');
    service.dispose();
    await expect(active.result).resolves.toMatchObject({ status: 'cancelled' });
    await expect(queued.result).resolves.toMatchObject({ status: 'cancelled' });
  });

  it('keeps a publisher cycle consumed before display rollback and makes later rollback inert', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    service.claimPublisherGptSlot({
      adUnitPath: '/network/slot',
      elementId: 'slot-div',
      initialLoadDisabled: false,
      sizes: [300, 250],
    });
    service.preparePublisherDisplay({ initialLoadDisabled: false, target: 'slot-div' });
    const decision = service.preparePublisherDisplay({
      initialLoadDisabled: false,
      target: 'slot-div',
    }) as Readonly<{ action: 'forward'; admission?: GoogletagPublisherCallAdmission }>;
    expect(decision.admission).toBeDefined();

    service.handleGptEvent('slotRequested', { slot });
    expect(service.snapshotForTest().cycles).toBe(1);
    decision.admission?.rollback();
    decision.admission?.commit();
    expect(service.snapshotForTest().cycles).toBe(1);

    service.handleGptEvent('slotRenderEnded', { isEmpty: false, slot });
    expect(service.snapshotForTest().cycles).toBe(0);
  });

  it('rolls back repeated display plus explicit and global refresh admissions without residue', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    const first = bindTrustedSlot(service, navigation, 'first');
    const second = bindTrustedSlot(service, navigation, 'second');
    for (const registeredSlotId of ['first', 'second'] as const) {
      service.claimPublisherGptSlot({
        adUnitPath: `/network/${registeredSlotId}`,
        elementId: `${registeredSlotId}-div`,
        initialLoadDisabled: false,
        sizes: [300, 250],
      });
      service.preparePublisherDisplay({
        initialLoadDisabled: false,
        target: `${registeredSlotId}-div`,
      });
    }

    for (let attempt = 0; attempt < 70; attempt += 1) {
      const display = service.preparePublisherDisplay({
        initialLoadDisabled: false,
        target: 'first-div',
      }) as Readonly<{ action: 'forward'; admission?: GoogletagPublisherCallAdmission }>;
      expect(display.admission).toBeDefined();
      display.admission?.rollback();
    }
    const explicit = service.preparePublisherRefresh({
      requestedSlots: Object.freeze([first]),
      slots: Object.freeze([first]),
    }) as Readonly<{ action: 'forward'; admission?: GoogletagPublisherCallAdmission }>;
    const global = service.preparePublisherRefresh({
      requestedSlots: undefined,
      slots: Object.freeze([first, second]),
    }) as Readonly<{ action: 'forward'; admission?: GoogletagPublisherCallAdmission }>;
    expect(explicit.admission).toBeDefined();
    expect(global.admission).toBeDefined();
    explicit.admission?.rollback();
    global.admission?.rollback();

    const firstRequest = service.request({
      intentId: 'after-rolled-back-explicit-refresh',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'first',
      requestClass: 'primary',
    });
    const secondRequest = service.request({
      intentId: 'after-rolled-back-global-refresh',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'second',
      requestClass: 'primary',
    });
    expect(firstRequest.status).toBe('active');
    expect(secondRequest.status).toBe('active');
  });

  it('commits a global refresh only for the publisher physicals snapshotted before native entry', async () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    const first = bindTrustedSlot(service, navigation, 'first');
    bindTrustedSlot(service, navigation, 'second');
    service.claimPublisherGptSlot({
      adUnitPath: '/network/first',
      elementId: 'first-div',
      initialLoadDisabled: false,
      sizes: [300, 250],
    });
    const global = service.preparePublisherRefresh({
      requestedSlots: undefined,
      slots: Object.freeze([first]),
    });
    expect(global.action).toBe('forward');
    if (global.action !== 'forward') throw new Error('Expected global refresh forwarding');
    expect(global.admission).toBeDefined();

    service.claimPublisherGptSlot({
      adUnitPath: '/network/second',
      elementId: 'second-div',
      initialLoadDisabled: false,
      sizes: [300, 250],
    });
    global.admission?.commit();

    const firstRequest = service.request({
      intentId: 'global-snapshot-first',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'first',
      requestClass: 'primary',
    });
    const secondRequest = service.request({
      intentId: 'global-snapshot-second',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'second',
      requestClass: 'primary',
    });
    await expect(firstRequest.result).resolves.toMatchObject({ reason: 'cycle_unattributable' });
    expect(secondRequest.status).toBe('active');
  });

  it('makes a pending publisher admission inert after navigation and service disposal', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const { navigation, runtime } = createRuntimeWithNavigation();
    bindTrustedSlot(service, navigation);
    service.claimPublisherGptSlot({
      adUnitPath: '/network/slot',
      elementId: 'slot-div',
      initialLoadDisabled: false,
      sizes: [300, 250],
    });
    service.preparePublisherDisplay({ initialLoadDisabled: false, target: 'slot-div' });
    const decision = service.preparePublisherDisplay({
      initialLoadDisabled: false,
      target: 'slot-div',
    });
    expect(decision.action).toBe('forward');
    if (decision.action !== 'forward') throw new Error('Expected display forwarding');
    expect(decision.admission).toBeDefined();

    runtime.dispose();
    expect(() => decision.admission?.commit()).not.toThrow();
    expect(() => decision.admission?.rollback()).not.toThrow();
    service.dispose();
    expect(() => decision.admission?.commit()).not.toThrow();
    expect(() => decision.admission?.rollback()).not.toThrow();
  });

  it('hydrates only one disconnected TS fallback with the configured prefix, path, and sizes', () => {
    const dom = createReconciliationBoundary();
    const firstElement = {};
    const secondElement = {};
    dom.put('slot-first', firstElement);
    dom.put('slot-second', secondElement);
    const warnPublisherHandoffMismatch = vi.fn();
    const service = createSlotService({
      googletag: createGptHarness().adapter,
      reconciliation: dom.boundary,
      warnPublisherHandoffMismatch,
    });
    const navigation = createNavigation();
    expect(
      service.register(navigation, [serverRegistration('first'), serverRegistration('second')])
    ).toMatchObject({ ok: true });
    const first = {};
    const second = {};
    for (const [id, slot] of [
      ['first', first],
      ['second', second],
    ] as const) {
      expect(
        service.adoptGptSlot(navigation.generation, id, {
          definition: {
            adUnitPath: '/network/hydrated',
            elementId: `slot-${id}`,
            sizes: Object.freeze([[300, 250]]),
          },
          elementIdPrefix: 'slot-',
          ownership: 'trusted_server',
          slot,
        })
      ).toEqual({ ok: true });
      dom.disconnect(`slot-${id}`);
    }

    const hydration = Object.freeze({
      adUnitPath: '/network/hydrated',
      elementId: 'slot-hydrated',
      initialLoadDisabled: false,
      sizes: Object.freeze([300, 250]),
    });
    expect(service.claimPublisherGptSlot(hydration)).toEqual({ action: 'forward' });
    expect(service.recordPublisherDestruction(second)).toBe(true);
    expect(
      service.claimPublisherGptSlot({ ...hydration, adUnitPath: '/network/mismatch' })
    ).toEqual({ action: 'forward' });
    expect(service.claimPublisherGptSlot({ ...hydration, sizes: [728, 90] })).toEqual({
      action: 'forward',
    });
    expect(service.claimPublisherGptSlot(hydration)).toEqual({ action: 'handoff', slot: first });
    expect(warnPublisherHandoffMismatch).not.toHaveBeenCalled();
  });

  it('suppresses the exact first explicit refresh after a disabled-load handoff', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    expect(
      service.claimPublisherGptSlot({
        adUnitPath: '/network/slot',
        elementId: 'slot-div',
        initialLoadDisabled: true,
        sizes: [300, 250],
      })
    ).toEqual({ action: 'handoff', slot });

    expect(
      service.preparePublisherRefresh({
        requestedSlots: Object.freeze([slot]),
        slots: Object.freeze([slot]),
      })
    ).toEqual({ action: 'suppress' });
    const forwarded = service.preparePublisherRefresh({
      requestedSlots: Object.freeze([slot]),
      slots: Object.freeze([slot]),
    });
    expect(forwarded.action).toBe('forward');
    if (forwarded.action === 'forward') {
      expect(forwarded.admission).toBeDefined();
      forwarded.admission?.commit();
    }
  });

  it('uses captured Set validation intrinsics on a hostile page', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    const originalHas = Set.prototype.has;
    const originalAdd = Set.prototype.add;
    Set.prototype.has = function (): boolean {
      throw new Error('poisoned has');
    } as typeof Set.prototype.has;
    Set.prototype.add = function (): Set<unknown> {
      throw new Error('poisoned add');
    } as typeof Set.prototype.add;
    let result: ReturnType<typeof service.register> | undefined;
    try {
      result = service.register(navigation, [
        serverRegistration('captured', { domAliases: ['captured-div'] }),
      ]);
    } finally {
      Set.prototype.has = originalHas;
      Set.prototype.add = originalAdd;
    }
    expect(result).toMatchObject({ ok: true });
    expect(service.resolveDomAlias('captured-div')?.registeredSlotId).toBe('captured');
  });

  it('rolls back GPT identity publication when ownership becomes stale during adoption', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const { navigation, runtime } = createRuntimeWithNavigation();
    expect(service.register(navigation, [serverRegistration('old')])).toMatchObject({ ok: true });
    const slot = {};
    const racedBinding = Object.defineProperties(
      {},
      {
        definition: { value: undefined },
        ownership: {
          get: () => {
            runtime.replaceNavigation();
            return 'publisher';
          },
        },
        slot: { value: slot },
      }
    ) as GptSlotBinding;

    expect(service.adoptGptSlot(navigation.generation, 'old', racedBinding)).toEqual({
      ok: false,
      reason: 'stale_owner',
    });
    const next = runtime.currentNavigation;
    if (!next) throw new Error('Expected replacement navigation');
    expect(service.register(next, [serverRegistration('next')])).toMatchObject({ ok: true });
    expect(service.adoptGptSlot(next.generation, 'next', { ownership: 'publisher', slot })).toEqual(
      { ok: true }
    );
  });

  it('conditionally deletes a WeakMap identity published just before a stale-owner check', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    let phase: 'adopt' | 'register' | 'steady' = 'register';
    let adoptChecks = 0;
    const generation = {};
    const owner = {
      generation,
      isCurrent: () => {
        if (phase !== 'adopt') return true;
        adoptChecks += 1;
        return adoptChecks < 3;
      },
      onDispose: vi.fn(),
    } as unknown as NavigationSession;
    expect(service.register(owner, [serverRegistration('slot')])).toMatchObject({ ok: true });
    const slot = {};
    phase = 'adopt';

    expect(service.adoptGptSlot(generation, 'slot', { ownership: 'publisher', slot })).toEqual({
      ok: false,
      reason: 'stale_owner',
    });
    phase = 'steady';
    expect(service.adoptGptSlot(generation, 'slot', { ownership: 'publisher', slot })).toEqual({
      ok: true,
    });
  });
});

describe('navigation-owned DOM reconciliation', () => {
  afterEach(() => vi.useRealTimers());

  it('preserves the physical slot when DOM connectivity cannot be established', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const gpt = createGptHarness();
    const dom = createReconciliationBoundary();
    dom.put('slot-div', {});
    const service = createSlotService({
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: Object.freeze({
        ...dom.boundary,
        isConnected: () => {
          throw new Error('fictional DOM connectivity failure');
        },
      }),
    });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    service.activate();

    dom.trigger();
    await vi.advanceTimersByTimeAsync(5_000);

    expect(gpt.destroySlots).not.toHaveBeenCalled();
    expect(gpt.defineSlot).not.toHaveBeenCalled();
  });

  it('reconciles a TS slot whose original DOM element was already absent at adoption', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const gpt = createGptHarness();
    const dom = createReconciliationBoundary();
    const service = createSlotService({
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: dom.boundary,
    });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    service.activate();

    dom.put('slot-div', {});
    dom.trigger();
    await vi.advanceTimersByTimeAsync(250);

    expect(gpt.defineSlot).toHaveBeenCalledExactlyOnceWith(
      '/network/slot',
      [[300, 250]],
      'slot-div'
    );
  });

  it('debounces an exact disconnected TS slot through the 249/250 ms boundary', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const gpt = createGptHarness();
    const dom = createReconciliationBoundary();
    const disposeCommittedArtifact = vi.fn(() => {
      throw new Error('fictional artifact cleanup failure');
    });
    dom.put('slot-div', {});
    const service = createSlotService({
      disposeCommittedArtifact,
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: dom.boundary,
    });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    service.activate();

    dom.replace('slot-div', {});
    await vi.advanceTimersByTimeAsync(249);
    expect(gpt.destroySlots).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(1);
    expect(gpt.destroySlots).toHaveBeenCalledExactlyOnceWith([
      expect.objectContaining({ id: 'slot' }),
    ]);
    expect(gpt.defineSlot).toHaveBeenCalledExactlyOnceWith(
      '/network/slot',
      [[300, 250]],
      'slot-div'
    );
    expect(disposeCommittedArtifact).toHaveBeenCalledExactlyOnceWith(navigation.generation, 'slot');

    const request = service.request({
      intentId: 'after-rebind',
      navigationGeneration: navigation.generation,
      operation: 'display',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await vi.advanceTimersByTimeAsync(0);
    expect(request.status).toBe('active');
    expect(gpt.display).toHaveBeenCalledExactlyOnceWith(gpt.defineSlot.mock.results[0]?.value);
  });

  it('settles an invocation tied to the orphan before publishing the replacement', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const gpt = createGptHarness();
    const dom = createReconciliationBoundary();
    dom.put('slot-div', {});
    const service = createSlotService({
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: dom.boundary,
    });
    const navigation = createNavigation();
    const orphan = bindTrustedSlot(service, navigation);
    service.activate();
    const request = service.request({
      intentId: 'before-rebind',
      navigationGeneration: navigation.generation,
      operation: 'display',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await vi.advanceTimersByTimeAsync(0);
    expect(gpt.display).toHaveBeenCalledExactlyOnceWith(orphan);

    dom.replace('slot-div', {});
    await vi.advanceTimersByTimeAsync(250);
    await expect(request.result).resolves.toEqual({
      status: 'failed',
      reason: 'gpt_request_failed',
    });
    await vi.advanceTimersByTimeAsync(3_000);
    expect(gpt.destroySlots).toHaveBeenCalledExactlyOnceWith([orphan]);

    service.request({
      intentId: 'after-rebind',
      navigationGeneration: navigation.generation,
      operation: 'display',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await vi.advanceTimersByTimeAsync(0);
    expect(gpt.display).toHaveBeenLastCalledWith(gpt.defineSlot.mock.results[0]?.value);
  });

  it('runs one final unresolved pass at 5,000 ms and settles exact work', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const gpt = createGptHarness();
    const dom = createReconciliationBoundary();
    dom.put('slot-div', {});
    const service = createSlotService({
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: dom.boundary,
    });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    service.activate();
    dom.disconnect('slot-div');
    await vi.advanceTimersByTimeAsync(2_999);
    const request = service.request({
      intentId: 'orphaned',
      navigationGeneration: navigation.generation,
      operation: 'display',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });

    await vi.advanceTimersByTimeAsync(2_000);
    expect(request.status).toBe('active');
    expect(gpt.destroySlots).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(1);
    await expect(request.result).resolves.toEqual({ status: 'failed', reason: 'slot_unresolved' });
    expect(gpt.destroySlots).toHaveBeenCalledExactlyOnceWith([
      expect.objectContaining({ id: 'slot' }),
    ]);
    expect(gpt.defineSlot).not.toHaveBeenCalled();
  });

  it.each([
    ['unresolved', 'destroy_false'],
    ['unresolved', 'destroy_throw'],
    ['ambiguous', 'destroy_false'],
    ['ambiguous', 'destroy_throw'],
  ] as const)('settles final %s cleanup %s as gpt_request_failed', async (resolution, failure) => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const gpt = createGptHarness();
    if (failure === 'destroy_false') gpt.destroySlots.mockReturnValue(false);
    else {
      gpt.destroySlots.mockImplementation(() => {
        throw new Error('fictional destroy failure');
      });
    }
    const dom = createReconciliationBoundary();
    dom.put('slot-div', {});
    const service = createSlotService({
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: dom.boundary,
    });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    service.activate();
    if (resolution === 'ambiguous') dom.replaceAmbiguously('slot-div', [{}, {}]);
    else dom.disconnect('slot-div');
    await vi.advanceTimersByTimeAsync(4_999);
    const request = service.request({
      intentId: `${resolution}-${failure}`,
      navigationGeneration: navigation.generation,
      operation: 'display',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });

    await vi.advanceTimersByTimeAsync(1);

    await expect(request.result).resolves.toEqual({
      status: 'failed',
      reason: 'gpt_request_failed',
    });
    expect(gpt.destroySlots).toHaveBeenCalledExactlyOnceWith([
      expect.objectContaining({ id: 'slot' }),
    ]);
  });

  it('keeps final cleanup pending and lets navigation cancellation beat its late result', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const gpt = createGptHarness({ deferDestroyedResult: true });
    const dom = createReconciliationBoundary();
    dom.put('slot-div', {});
    const service = createSlotService({
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: dom.boundary,
    });
    const { navigation, runtime } = createRuntimeWithNavigation();
    const oldSlot = bindTrustedSlot(service, navigation);
    service.activate();
    dom.disconnect('slot-div');
    await vi.advanceTimersByTimeAsync(4_999);
    const request = service.request({
      intentId: 'navigation-wins-late-cleanup',
      navigationGeneration: navigation.generation,
      operation: 'display',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });

    vi.advanceTimersByTime(1);
    expect(request.status).toBe('active');
    expect(gpt.destroySlots).toHaveBeenCalledTimes(1);
    const nextResult = runtime.replaceNavigation();
    expect(nextResult.ok).toBe(true);
    if (!nextResult.ok) throw new Error('Expected replacement navigation');
    const next = nextResult.value;
    await expect(request.result).resolves.toEqual({
      status: 'cancelled',
      reason: 'navigation_disposed',
    });
    expect(
      service.register(next, [
        serverRegistration('slot', {
          adUnitCode: '/network/slot',
          domAliases: ['slot-div'],
        }),
      ])
    ).toEqual({ ok: false, reason: 'slot_quarantined' });

    gpt.resolveDeferredDestroyed();
    await Promise.resolve();
    await Promise.resolve();

    expect(request.status).toBe('terminal');
    expect(gpt.destroySlots).toHaveBeenCalledTimes(1);
    const replacement = bindTrustedSlot(service, next);
    gpt.resolveDeferredDestroyed();
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'late-old-slot',
      slot: oldSlot,
    });
    expect(service.recordPublisherDestruction(oldSlot)).toBe(false);
    expect(service.isBoundGptSlot(next.generation, 'slot', replacement)).toBe(true);
  });

  it('lets request supersession win while final cleanup completes later', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const gpt = createGptHarness({ synchronousRun: false });
    const dom = createReconciliationBoundary();
    dom.put('slot-div', {});
    const service = createSlotService({
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: dom.boundary,
    });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    service.activate();
    dom.disconnect('slot-div');
    await vi.advanceTimersByTimeAsync(4_999);
    const request = service.request({
      intentId: 'supersession-wins-late-cleanup',
      navigationGeneration: navigation.generation,
      operation: 'display',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });

    vi.advanceTimersByTime(1);
    expect(request.status).toBe('active');
    request.dispose();
    await expect(request.result).resolves.toEqual({
      status: 'cancelled',
      reason: 'superseded',
    });
    await Promise.resolve();
    await Promise.resolve();

    expect(request.status).toBe('terminal');
    expect(gpt.destroySlots).toHaveBeenCalledTimes(1);
  });

  it('releases the exact committed artifact before retiring a failed reconciliation', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const gpt = createGptHarness();
    const dom = createReconciliationBoundary();
    const disposeCommittedArtifact = vi.fn();
    dom.put('slot-div', {});
    const service = createSlotService({
      disposeCommittedArtifact,
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: dom.boundary,
    });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    service.activate();

    dom.disconnect('slot-div');
    await vi.advanceTimersByTimeAsync(5_000);

    expect(disposeCommittedArtifact).toHaveBeenCalledExactlyOnceWith(navigation.generation, 'slot');
    expect(disposeCommittedArtifact.mock.invocationCallOrder[0]).toBeLessThan(
      gpt.destroySlots.mock.invocationCallOrder[0] as number
    );
    expect(gpt.destroySlots).toHaveBeenCalledOnce();
  });

  it('commits a unique replacement found only by the final 5,000 ms pass', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const gpt = createGptHarness();
    const dom = createReconciliationBoundary();
    dom.put('slot-div', {});
    const service = createSlotService({
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: dom.boundary,
    });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    service.activate();

    dom.disconnect('slot-div');
    await vi.advanceTimersByTimeAsync(250);
    expect(gpt.defineSlot).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(4_749);
    dom.put('slot-div', {});
    expect(gpt.defineSlot).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(1);
    expect(gpt.defineSlot).toHaveBeenCalledExactlyOnceWith(
      '/network/slot',
      [[300, 250]],
      'slot-div'
    );
    expect(gpt.destroySlots).toHaveBeenCalledTimes(1);
  });

  it('keeps an ambiguous replacement unresolved through the final pass', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const gpt = createGptHarness();
    const dom = createReconciliationBoundary();
    dom.put('slot-div', {});
    const service = createSlotService({
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: dom.boundary,
    });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    service.activate();
    dom.replaceAmbiguously('slot-div', [{}, {}]);
    await vi.advanceTimersByTimeAsync(2_999);
    const request = service.request({
      intentId: 'ambiguous',
      navigationGeneration: navigation.generation,
      operation: 'display',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });

    await vi.advanceTimersByTimeAsync(2_001);
    await expect(request.result).resolves.toEqual({ status: 'failed', reason: 'slot_unresolved' });
    expect(gpt.defineSlot).not.toHaveBeenCalled();
    expect(gpt.destroySlots).toHaveBeenCalledTimes(1);
  });

  it.each(['destroy_false', 'destroy_throw', 'define'] as const)(
    'settles %s transaction failure as gpt_request_failed without a second physical slot',
    async (failure) => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const gpt = createGptHarness();
      if (failure === 'destroy_false') gpt.destroySlots.mockReturnValue(false);
      else if (failure === 'destroy_throw') {
        gpt.destroySlots.mockImplementation(() => {
          throw new Error('fictional destroy failure');
        });
      } else gpt.defineSlot.mockReturnValueOnce(undefined);
      const dom = createReconciliationBoundary();
      dom.put('slot-div', {});
      const service = createSlotService({
        googletag: gpt.adapter,
        now: () => Date.now(),
        reconciliation: dom.boundary,
      });
      const navigation = createNavigation();
      bindTrustedSlot(service, navigation);
      service.activate();
      const request = service.request({
        intentId: `failed-${failure}`,
        navigationGeneration: navigation.generation,
        operation: 'display',
        registeredSlotId: 'slot',
        requestClass: 'primary',
      });
      await vi.advanceTimersByTimeAsync(0);

      dom.replace('slot-div', {});
      await vi.advanceTimersByTimeAsync(250);
      await expect(request.result).resolves.toEqual({
        status: 'failed',
        reason: 'gpt_request_failed',
      });
      expect(gpt.destroySlots).toHaveBeenCalledTimes(1);
      expect(gpt.defineSlot).toHaveBeenCalledTimes(failure === 'define' ? 1 : 0);
      expect(service.snapshotForTest().physicalSlots).toBe(0);
    }
  );

  it('quarantines an exact replacement candidate the adapter could not destroy', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const orphan = Object.freeze({ orphan: true });
    const gpt = createGptHarness({ orphanOnReplace: orphan });
    const dom = createReconciliationBoundary();
    dom.put('slot-div', {});
    const service = createSlotService({
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: dom.boundary,
    });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    service.activate();
    const request = service.request({
      intentId: 'orphaned-replacement',
      navigationGeneration: navigation.generation,
      operation: 'display',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await vi.advanceTimersByTimeAsync(0);

    dom.replace('slot-div', {});
    await vi.advanceTimersByTimeAsync(250);
    await expect(request.result).resolves.toEqual({
      status: 'failed',
      reason: 'gpt_request_failed',
    });
    const replacementBinding = {
      definition: {
        adUnitPath: '/network/slot',
        elementId: 'slot-div',
        sizes: Object.freeze([[300, 250]]),
      },
      ownership: 'trusted_server' as const,
      slot: Object.freeze({ replacementAfterOrphan: true }),
    };
    expect(service.adoptGptSlot(navigation.generation, 'slot', replacementBinding)).toEqual({
      ok: false,
      reason: 'slot_quarantined',
    });

    expect(service.recordPublisherDestruction(orphan)).toBe(true);
    expect(service.adoptGptSlot(navigation.generation, 'slot', replacementBinding)).toEqual({
      ok: true,
    });
  });

  it('lets expiry beat a final-pass replacement that cannot commit synchronously', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const gpt = createGptHarness({ synchronousRun: false });
    const dom = createReconciliationBoundary();
    dom.put('slot-div', {});
    const service = createSlotService({
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: dom.boundary,
    });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    service.activate();
    await Promise.resolve();

    dom.disconnect('slot-div');
    await vi.advanceTimersByTimeAsync(250);
    await vi.advanceTimersByTimeAsync(4_749);
    dom.put('slot-div', {});
    const request = service.request({
      intentId: 'expiry-wins',
      navigationGeneration: navigation.generation,
      operation: 'display',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });

    await vi.advanceTimersByTimeAsync(1);
    await expect(request.result).resolves.toEqual({
      status: 'failed',
      reason: 'slot_unresolved',
    });
    expect(gpt.defineSlot).not.toHaveBeenCalled();
    expect(gpt.destroySlots).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it('lets publisher ownership transfer cancel a queued reconciliation transaction', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const gpt = createGptHarness({ synchronousRun: false });
    const dom = createReconciliationBoundary();
    dom.put('slot-div', {});
    const service = createSlotService({
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: dom.boundary,
    });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    service.activate();
    await Promise.resolve();

    dom.replace('slot-div', {});
    vi.advanceTimersByTime(250);
    expect(
      service.adoptGptSlot(navigation.generation, 'slot', {
        ownership: 'publisher',
        slot,
      })
    ).toEqual({ ok: true });
    await Promise.resolve();
    await Promise.resolve();

    expect(gpt.defineSlot).not.toHaveBeenCalled();
    expect(gpt.destroySlots).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('allows two successful rebinds and fails a third disconnect immediately', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const gpt = createGptHarness();
    const dom = createReconciliationBoundary();
    dom.put('slot-div', {});
    const service = createSlotService({
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: dom.boundary,
    });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    service.activate();

    dom.replace('slot-div', {});
    await vi.advanceTimersByTimeAsync(250);
    dom.replace('slot-div', {});
    await vi.advanceTimersByTimeAsync(250);
    expect(gpt.defineSlot).toHaveBeenCalledTimes(2);

    const request = service.request({
      intentId: 'capacity',
      navigationGeneration: navigation.generation,
      operation: 'display',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    dom.disconnect('slot-div');
    await expect(request.result).resolves.toEqual({
      status: 'failed',
      reason: 'reconciliation_capacity',
    });
    expect(gpt.defineSlot).toHaveBeenCalledTimes(2);
    expect(gpt.destroySlots).toHaveBeenCalledTimes(3);
  });

  it('cancels reconciliation on publisher transfer and disconnects with navigation', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const gpt = createGptHarness();
    const dom = createReconciliationBoundary();
    dom.put('slot-div', {});
    const service = createSlotService({
      googletag: gpt.adapter,
      now: () => Date.now(),
      reconciliation: dom.boundary,
    });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    service.activate();
    dom.disconnect('slot-div');

    expect(
      service.adoptGptSlot(navigation.generation, 'slot', {
        ownership: 'publisher',
        slot,
      })
    ).toEqual({ ok: true });
    await vi.advanceTimersByTimeAsync(5_000);
    expect(gpt.destroySlots).not.toHaveBeenCalled();

    navigation.dispose();
    expect(dom.observe).toHaveBeenCalledTimes(1);
    dom.trigger();
    await vi.runAllTimersAsync();
    expect(gpt.destroySlots).not.toHaveBeenCalled();
  });
});

describe('browser reconciliation boundary', () => {
  it('resolves only one exact connected element and releases its observer', async () => {
    const boundary = createBrowserSlotReconciliationBoundary(document, MutationObserver);
    expect(boundary).toBeDefined();
    if (!boundary) throw new Error('Expected the browser reconciliation boundary');
    const host = document.createElement('section');
    const first = document.createElement('div');
    first.id = 'tsjs-reconciliation-exact';
    host.append(first);
    document.body.append(host);
    const callback = vi.fn();
    const release = boundary.observe(callback);

    expect(boundary.resolve(['tsjs-reconciliation-exact'])).toEqual({
      status: 'unique',
      element: first,
      elementId: 'tsjs-reconciliation-exact',
    });
    expect(boundary.isConnected(first)).toBe(true);

    const duplicate = document.createElement('div');
    duplicate.id = first.id;
    host.append(duplicate);
    await vi.waitFor(() => expect(callback).toHaveBeenCalled());
    expect(boundary.resolve([first.id])).toEqual({ status: 'ambiguous' });

    const callsBeforeRelease = callback.mock.calls.length;
    release();
    host.remove();
    await Promise.resolve();
    expect(callback).toHaveBeenCalledTimes(callsBeforeRelease);
    expect(boundary.isConnected(first)).toBe(false);
    expect(boundary.resolve([first.id])).toEqual({ status: 'unresolved' });
  });
});

function createReplacementHarness() {
  const replacement = { addService: vi.fn() };
  const destroySlots = vi.fn((_slots: readonly object[]) => true);
  const defineSlot = vi.fn((): object | undefined => replacement);
  const pubads = {
    addEventListener: vi.fn(),
    getSlots: () => [],
    refresh: vi.fn(),
    removeEventListener: vi.fn(),
  };
  const adapter = createBrowserGoogletagAdapter({
    googletag: {
      apiReady: true,
      cmd: { push: (command: () => void) => command() },
      defineSlot,
      destroySlots,
      display: vi.fn(),
      pubads: () => pubads,
      pubadsReady: true,
    },
  });
  return { adapter, defineSlot, destroySlots, pubads, replacement };
}

describe('adapter-owned GPT replacement transaction', () => {
  const definition = Object.freeze({
    adUnitPath: '/network/slot',
    elementId: 'slot-div',
    sizes: Object.freeze([[300, 250]]),
  });
  const commitReplacement = () => Object.freeze({ commit: () => true, rollback: vi.fn() });

  it.each(['throw', 'false'] as const)(
    'never publishes a second physical slot after %s failure',
    async (failure) => {
      const harness = createReplacementHarness();
      if (failure === 'throw') {
        harness.destroySlots.mockImplementation(() => {
          throw new Error('destroy failed');
        });
      } else if (failure === 'false') {
        harness.destroySlots.mockReturnValue(false);
      }
      const operation = harness.adapter.run((gpt) =>
        gpt.transactionalReplace({}, definition, () => true, commitReplacement)
      );

      await expect(operation.result).rejects.toBeDefined();
      expect(harness.defineSlot).not.toHaveBeenCalled();
      expect(harness.replacement.addService).not.toHaveBeenCalled();
    }
  );

  it.each([
    ['after-destroy', 1, 0, 1],
    ['after-define', 2, 1, 2],
    ['after-addService', 3, 1, 2],
  ] as const)(
    'checks stale generation %s and cleans any newly-defined object',
    async (_site, staleAt, expectedDefinitions, expectedDestroys) => {
      const harness = createReplacementHarness();
      let checks = 0;
      const operation = harness.adapter.run((gpt) =>
        gpt.transactionalReplace(
          {},
          definition,
          () => {
            checks += 1;
            return checks < staleAt;
          },
          commitReplacement
        )
      );

      await expect(operation.result).resolves.toEqual({ status: 'destroyed' });
      expect(harness.defineSlot).toHaveBeenCalledTimes(expectedDefinitions);
      expect(harness.destroySlots).toHaveBeenCalledTimes(expectedDestroys);
      expect(harness.replacement.addService).toHaveBeenCalledTimes(staleAt === 3 ? 1 : 0);
    }
  );

  it('surfaces failure to destroy a newly-defined stale replacement', async () => {
    const harness = createReplacementHarness();
    harness.destroySlots.mockReturnValueOnce(true).mockReturnValueOnce(false);
    let checks = 0;
    const operation = harness.adapter.run((gpt) =>
      gpt.transactionalReplace(
        {},
        definition,
        () => {
          checks += 1;
          return checks < 2;
        },
        commitReplacement
      )
    );

    await expect(operation.result).rejects.toBeDefined();
    expect(harness.destroySlots).toHaveBeenCalledTimes(2);
  });

  it('normalizes a defineSlot throw after destroying the old slot', async () => {
    const harness = createReplacementHarness();
    const publisherFailure = new Error('publisher define failed');
    harness.defineSlot.mockImplementation(() => {
      throw publisherFailure;
    });
    const operation = harness.adapter.run((gpt) =>
      gpt.transactionalReplace({}, definition, () => true, commitReplacement)
    );

    await expect(operation.result).rejects.toMatchObject({
      cause: publisherFailure,
      code: 'gpt_replacement_failed',
      oldSlotDestroyed: true,
      orphanedSlot: undefined,
    });
    expect(harness.destroySlots).toHaveBeenCalledOnce();
  });

  it('normalizes a generation callback throw after destroying the old slot', async () => {
    const harness = createReplacementHarness();
    const ownerFailure = new Error('generation check failed');
    const operation = harness.adapter.run((gpt) =>
      gpt.transactionalReplace(
        {},
        definition,
        () => {
          throw ownerFailure;
        },
        commitReplacement
      )
    );

    await expect(operation.result).rejects.toMatchObject({
      cause: ownerFailure,
      code: 'gpt_replacement_failed',
      oldSlotDestroyed: true,
      orphanedSlot: undefined,
    });
    expect(harness.defineSlot).not.toHaveBeenCalled();
    expect(harness.destroySlots).toHaveBeenCalledOnce();
  });

  it('normalizes commit-admission throws and destroys the exact uncommitted candidate', async () => {
    const harness = createReplacementHarness();
    const admissionFailure = new Error('commit admission failed');
    const operation = harness.adapter.run((gpt) =>
      gpt.transactionalReplace(
        {},
        definition,
        () => true,
        () => {
          throw admissionFailure;
        }
      )
    );

    await expect(operation.result).rejects.toMatchObject({
      cause: admissionFailure,
      code: 'gpt_replacement_failed',
      oldSlotDestroyed: true,
      orphanedSlot: undefined,
    });
    expect(harness.replacement.addService).not.toHaveBeenCalled();
    expect(harness.destroySlots).toHaveBeenCalledTimes(2);
    expect(harness.destroySlots).toHaveBeenNthCalledWith(2, [harness.replacement]);
  });

  it('leaves the service unbound after the real adapter destroys old then defineSlot throws', async () => {
    vi.useFakeTimers();
    const harness = createReplacementHarness();
    harness.defineSlot.mockImplementation(() => {
      throw new Error('publisher define failed');
    });
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const oldSlot = { old: true };
    expect(
      service.register(navigation, [
        serverRegistration('slot', {
          adUnitCode: '/network/slot',
          domAliases: ['slot-div'],
        }),
      ])
    ).toMatchObject({ ok: true });
    expect(
      service.adoptGptSlot(navigation.generation, 'slot', {
        definition,
        ownership: 'trusted_server',
        slot: oldSlot,
      })
    ).toEqual({ ok: true });
    const request = service.request({
      intentId: 'real-define-throw',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(3_000);
    await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
    await Promise.resolve();

    expect(service.recordPublisherDestruction(oldSlot)).toBe(false);
    expect(
      service.adoptGptSlot(navigation.generation, 'slot', {
        definition,
        ownership: 'trusted_server',
        slot: { retry: true },
      })
    ).toEqual({ ok: true });
  });

  it('leaves the service unbound when its generation check throws after old-slot destruction', async () => {
    vi.useFakeTimers();
    const harness = createReplacementHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const oldSlot = { old: true };
    let oldSlotDestroyed = false;
    const ownerFailure = new Error('owner check failed after destroy');
    const isCurrent = vi.spyOn(navigation, 'isCurrent').mockImplementation(() => {
      if (oldSlotDestroyed) throw ownerFailure;
      return true;
    });
    harness.destroySlots.mockImplementation((slots) => {
      if (slots[0] === oldSlot) oldSlotDestroyed = true;
      return true;
    });
    expect(
      service.register(navigation, [
        serverRegistration('slot', {
          adUnitCode: '/network/slot',
          domAliases: ['slot-div'],
        }),
      ])
    ).toMatchObject({ ok: true });
    expect(
      service.adoptGptSlot(navigation.generation, 'slot', {
        definition,
        ownership: 'trusted_server',
        slot: oldSlot,
      })
    ).toEqual({ ok: true });
    const request = service.request({
      intentId: 'real-current-throw',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(3_000);
    await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
    await Promise.resolve();

    expect(service.recordPublisherDestruction(oldSlot)).toBe(false);
    isCurrent.mockImplementation(() => true);
    expect(
      service.adoptGptSlot(navigation.generation, 'slot', {
        definition,
        ownership: 'trusted_server',
        slot: { retry: true },
      })
    ).toEqual({ ok: true });
  });

  it('rejects a defineSlot candidate that is the retired old object', async () => {
    const harness = createReplacementHarness();
    const oldSlot = { addService: vi.fn() };
    harness.defineSlot.mockReturnValue(oldSlot);
    const commit = vi.fn();
    const operation = harness.adapter.run((gpt) =>
      (
        gpt.transactionalReplace as unknown as (
          old: object,
          candidateDefinition: GoogletagReplacementDefinition,
          current: () => boolean,
          prepareCommit: (candidate: object) => { commit: () => boolean; rollback: () => void }
        ) => unknown
      )(
        oldSlot,
        definition,
        () => true,
        () => ({ commit, rollback: vi.fn() })
      )
    );

    await expect(operation.result).rejects.toBeDefined();
    expect(commit).not.toHaveBeenCalled();
    expect(harness.replacement.addService).not.toHaveBeenCalled();
  });

  it('surfaces the reused old identity when rejecting it cannot clean it up', async () => {
    const harness = createReplacementHarness();
    const oldSlot = { addService: vi.fn() };
    harness.defineSlot.mockReturnValue(oldSlot);
    harness.destroySlots.mockReturnValueOnce(true).mockReturnValueOnce(false);
    const operation = harness.adapter.run((gpt) =>
      gpt.transactionalReplace(oldSlot, definition, () => true, commitReplacement)
    );

    await expect(operation.result).rejects.toMatchObject({
      code: 'gpt_replacement_failed',
      oldSlotDestroyed: true,
      orphanedSlot: oldSlot,
    });
    expect(harness.destroySlots).toHaveBeenCalledTimes(2);
  });

  it('rolls back a synchronous service commit when the post-commit generation check is stale', async () => {
    const harness = createReplacementHarness();
    let checks = 0;
    let bound: object | undefined;
    const rollback = vi.fn(() => {
      bound = undefined;
    });
    const operation = harness.adapter.run((gpt) =>
      (
        gpt.transactionalReplace as unknown as (
          old: object,
          candidateDefinition: GoogletagReplacementDefinition,
          current: () => boolean,
          prepareCommit: (candidate: object) => { commit: () => boolean; rollback: () => void }
        ) => unknown
      )(
        {},
        definition,
        () => {
          checks += 1;
          return checks < 4;
        },
        (candidate) => ({
          commit: () => {
            bound = candidate;
            return true;
          },
          rollback,
        })
      )
    );

    await expect(operation.result).resolves.toEqual({ status: 'destroyed' });
    expect(rollback).toHaveBeenCalledOnce();
    expect(bound).toBeUndefined();
    expect(harness.destroySlots).toHaveBeenCalledTimes(2);
  });

  it('surfaces the exact orphan candidate when post-commit cleanup cannot destroy it', async () => {
    const harness = createReplacementHarness();
    harness.destroySlots.mockReturnValueOnce(true).mockReturnValueOnce(false);
    const rollback = vi.fn();
    let checks = 0;
    const operation = harness.adapter.run((gpt) =>
      (
        gpt.transactionalReplace as unknown as (
          old: object,
          candidateDefinition: GoogletagReplacementDefinition,
          current: () => boolean,
          prepareCommit: (candidate: object) => { commit: () => boolean; rollback: () => void }
        ) => unknown
      )(
        {},
        definition,
        () => {
          checks += 1;
          return checks < 4;
        },
        () => ({ commit: () => true, rollback })
      )
    );

    await expect(operation.result).rejects.toMatchObject({
      code: 'gpt_replacement_failed',
      orphanedSlot: harness.replacement,
    });
    expect(rollback).toHaveBeenCalledOnce();
  });
});

function readyListenerBinding() {
  const addEventListener = vi.fn();
  const removeEventListener = vi.fn();
  const pubads = {
    addEventListener,
    getSlots: () => [],
    refresh: vi.fn(),
    removeEventListener,
  };
  return {
    addEventListener,
    binding: {
      apiReady: true,
      cmd: { push: (command: () => void) => command() },
      display: vi.fn(),
      pubads: () => pubads,
      pubadsReady: true,
    },
    removeEventListener,
  };
}

describe('binding-aware GPT listener activation', () => {
  it('installs observation without timers and starts readiness only after commit', async () => {
    vi.useFakeTimers();
    const target: { googletag?: unknown } = {};
    const adapter = createBrowserGoogletagAdapter(target);
    const service = createSlotService({ googletag: adapter });
    service.activate();
    expect(vi.getTimerCount()).toBe(0);

    const missing = service.start();
    expect(vi.getTimerCount()).toBe(1);
    await vi.advanceTimersByTimeAsync(10_000);
    await expect(missing.result).rejects.toMatchObject({ code: 'external_ready_timeout' });

    const ready = readyListenerBinding();
    target.googletag = ready.binding;
    await expect(service.start().result).resolves.toBeUndefined();
    await expect(service.start().result).resolves.toBeUndefined();

    expect(ready.addEventListener.mock.calls.map(([type]) => type)).toEqual([
      'slotRequested',
      'slotRenderEnded',
    ]);
  });

  it('subscribes a replacement binding before allowing later operations without duplicating either', async () => {
    const first = readyListenerBinding();
    const second = readyListenerBinding();
    const target: { googletag?: unknown } = { googletag: first.binding };
    const adapter = createBrowserGoogletagAdapter(target);
    const service = createSlotService({ googletag: adapter });
    service.activate();
    await expect(service.start().result).resolves.toBeUndefined();
    target.googletag = second.binding;
    await expect(service.start().result).resolves.toBeUndefined();
    await expect(service.start().result).resolves.toBeUndefined();

    expect(first.addEventListener).toHaveBeenCalledTimes(2);
    expect(second.addEventListener).toHaveBeenCalledTimes(2);
    expect(first.removeEventListener).toHaveBeenCalledTimes(2);
    service.dispose();
    expect(first.removeEventListener).toHaveBeenCalledTimes(2);
    expect(second.removeEventListener).toHaveBeenCalledTimes(2);
  });
});

describe('physical GPT cycles', () => {
  afterEach(() => vi.useRealTimers());

  it('preserves external_queue_full when GPT readiness admission is saturated', async () => {
    vi.useFakeTimers();
    const target: { googletag?: unknown } = {};
    const adapter = createBrowserGoogletagAdapter(target);
    const service = createSlotService({ googletag: adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    for (let index = 0; index < 64; index += 1) {
      const queued = adapter.run(() => undefined);
      void queued.result.catch(() => undefined);
    }

    const request = service.request({
      intentId: 'queue-capacity',
      navigationGeneration: navigation.generation,
      operation: 'display',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });

    await expect(request.result).resolves.toEqual({
      status: 'failed',
      reason: 'external_queue_full',
    });
    service.dispose();
    adapter.dispose();
  });

  it('preserves external_ready_timeout when GPT never becomes ready', async () => {
    vi.useFakeTimers();
    const target: { googletag?: unknown } = {};
    const adapter = createBrowserGoogletagAdapter(target);
    const service = createSlotService({ googletag: adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    const request = service.request({
      intentId: 'readiness-deadline',
      navigationGeneration: navigation.generation,
      operation: 'display',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });

    await vi.advanceTimersByTimeAsync(10_000);

    await expect(request.result).resolves.toEqual({
      status: 'failed',
      reason: 'external_ready_timeout',
    });
    service.dispose();
    adapter.dispose();
  });

  it('records intent before a synchronous slotRequested event and supports SRA per slot', async () => {
    vi.useFakeTimers();
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const first = bindTrustedSlot(service, navigation, 'first');
    const second = bindTrustedSlot(service, navigation, 'second');
    harness.display.mockImplementation((slot: object) => {
      service.handleGptEvent('slotRequested', { slot });
    });

    const firstRequest = service.request({
      intentId: 'intent-first',
      navigationGeneration: navigation.generation,
      operation: 'display',
      requestClass: 'primary',
      registeredSlotId: 'first',
    });
    const secondRequest = service.request({
      intentId: 'intent-second',
      navigationGeneration: navigation.generation,
      operation: 'display',
      requestClass: 'primary',
      registeredSlotId: 'second',
    });
    await Promise.resolve();
    expect(harness.display.mock.calls.map(([slot]) => slot)).toEqual([first, second]);

    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'response-first',
      slot: first,
    });
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: true,
      responseIdentifier: 'response-second',
      slot: second,
    });
    await expect(firstRequest.result).resolves.toEqual({
      responseIdentifier: 'response-first',
      status: 'rendered',
    });
    await expect(secondRequest.result).resolves.toEqual({
      responseIdentifier: 'response-second',
      status: 'empty',
    });
  });

  it('uses display only for registration under disabled initial load and one exact refresh', async () => {
    const harness = createGptHarness({ initialLoadDisabled: true });
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);

    service.request({
      intentId: 'intent',
      navigationGeneration: navigation.generation,
      operation: 'display',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await Promise.resolve();

    expect(harness.display).toHaveBeenCalledExactlyOnceWith(slot);
    expect(harness.refresh).toHaveBeenCalledExactlyOnceWith(
      [slot],
      Object.freeze({ changeCorrelator: false })
    );
  });

  it('treats a slotRequested raised by disabled-load display as publisher overlap and skips refresh', async () => {
    const harness = createGptHarness({ initialLoadDisabled: true });
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    harness.display.mockImplementation(() => {
      service.handleGptEvent('slotRequested', { slot });
    });
    const request = service.request({
      intentId: 'display-overlap',
      navigationGeneration: navigation.generation,
      operation: 'display',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });

    await expect(request.result).resolves.toEqual({
      reason: 'cycle_unattributable',
      status: 'failed',
    });
    expect(harness.refresh).not.toHaveBeenCalled();
  });

  it('fails a disabled-initial-load request when refresh throws', async () => {
    const harness = createGptHarness({ initialLoadDisabled: true });
    harness.refresh.mockImplementation(() => {
      throw new Error('refresh unavailable');
    });
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);

    const request = service.request({
      intentId: 'intent',
      navigationGeneration: navigation.generation,
      operation: 'display',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });

    await expect(request.result).resolves.toEqual({
      reason: 'gpt_request_failed',
      status: 'failed',
    });
  });

  it('fails a disabled-initial-load request when refresh is unavailable', async () => {
    const harness = createGptHarness({ initialLoadDisabled: true, missingRefresh: true });
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    const request = service.request({
      intentId: 'missing-refresh',
      navigationGeneration: navigation.generation,
      operation: 'display',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });

    await expect(request.result).resolves.toEqual({
      reason: 'gpt_request_failed',
      status: 'failed',
    });
  });

  it('records every SRA intent before one refresh and fans out events by object identity', async () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const first = bindTrustedSlot(service, navigation, 'sra-first');
    const second = bindTrustedSlot(service, navigation, 'sra-second');

    const requests = service.requestBatch([
      {
        intentId: 'sra-intent-first',
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        requestClass: 'primary',
        registeredSlotId: 'sra-first',
      },
      {
        intentId: 'sra-intent-second',
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        requestClass: 'primary',
        registeredSlotId: 'sra-second',
      },
    ]);
    await Promise.resolve();
    expect(harness.refresh).toHaveBeenCalledExactlyOnceWith(
      [first, second],
      Object.freeze({ changeCorrelator: false })
    );
    service.handleGptEvent('slotRequested', { slot: first });
    service.handleGptEvent('slotRequested', { slot: second });
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'sra-first-response',
      slot: first,
    });
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: true,
      responseIdentifier: 'sra-second-response',
      slot: second,
    });
    await expect(Promise.all(requests.map(({ result }) => result))).resolves.toEqual([
      { responseIdentifier: 'sra-first-response', status: 'rendered' },
      { responseIdentifier: 'sra-second-response', status: 'empty' },
    ]);
  });

  it('rejects display batches at the type and runtime boundaries before mutation', () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    const displayBatch = [
      {
        intentId: 'valid-before-display',
        navigationGeneration: navigation.generation,
        operation: 'refresh' as const,
        registeredSlotId: 'slot',
        requestClass: 'primary',
      },
      {
        intentId: 'display-batch',
        navigationGeneration: navigation.generation,
        operation: 'display' as const,
        registeredSlotId: 'slot',
        requestClass: 'primary',
      },
    ] as const;
    const compileOnly = (): void => {
      // @ts-expect-error requestBatch is refresh-only; single request retains display support.
      service.requestBatch(displayBatch);
    };
    expect(compileOnly).toBeTypeOf('function');
    const inventory = service.snapshotForTest();
    const runtimeRequestBatch = service.requestBatch as unknown as (
      inputs: readonly object[]
    ) => unknown;

    expect(runtimeRequestBatch(displayBatch)).toEqual([]);
    expect(service.snapshotForTest()).toEqual(inventory);
    expect(harness.display).not.toHaveBeenCalled();
    expect(harness.refresh).not.toHaveBeenCalled();
  });

  it.each(['unknown-slot', 'duplicate-slot', 'duplicate-intent', 'mixed-navigation'] as const)(
    'prevalidates the entire SRA batch atomically: %s',
    (failure) => {
      const harness = createGptHarness();
      const service = createSlotService({ googletag: harness.adapter });
      const firstNavigation = createNavigation();
      const secondNavigation = createNavigation();
      bindTrustedSlot(service, firstNavigation, 'first');
      bindTrustedSlot(service, firstNavigation, 'second');
      bindTrustedSlot(service, secondNavigation, 'other-navigation');
      const first = {
        intentId: 'first-intent',
        navigationGeneration: firstNavigation.generation,
        operation: 'refresh' as const,
        registeredSlotId: 'first',
        requestClass: 'primary',
      };
      const second = {
        intentId: failure === 'duplicate-intent' ? first.intentId : 'second-intent',
        navigationGeneration:
          failure === 'mixed-navigation' ? secondNavigation.generation : firstNavigation.generation,
        operation: 'refresh' as const,
        registeredSlotId:
          failure === 'unknown-slot'
            ? 'missing'
            : failure === 'duplicate-slot'
              ? first.registeredSlotId
              : failure === 'mixed-navigation'
                ? 'other-navigation'
                : 'second',
        requestClass: 'primary',
      };
      const inventory = service.snapshotForTest();

      expect(service.requestBatch([first, second])).toEqual([]);
      expect(service.snapshotForTest()).toEqual(inventory);
      expect(harness.refresh).not.toHaveBeenCalled();
    }
  );

  it('treats an empty SRA batch as an inert rejection', () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    expect(service.requestBatch([])).toEqual([]);
    expect(service.snapshotForTest()).toEqual({
      cycles: 0,
      intents: 0,
      physicalSlots: 0,
      records: 0,
    });
    expect(harness.refresh).not.toHaveBeenCalled();
  });

  it('contains a throwing batch length read before validation', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const hostileInputs = new Proxy([], {
      get: (target, key, receiver) => {
        if (key === 'length') throw new Error('hostile batch length');
        return Reflect.get(target, key, receiver);
      },
    });
    const runtimeRequestBatch = service.requestBatch as unknown as (
      inputs: readonly object[]
    ) => unknown;
    let outcome: unknown;

    expect(() => {
      outcome = runtimeRequestBatch(hostileInputs);
    }).not.toThrow();
    expect(outcome).toEqual([]);
    expect(service.snapshotForTest()).toEqual({
      cycles: 0,
      intents: 0,
      physicalSlots: 0,
      records: 0,
    });
  });

  it('does not leak partial admission through a poisoned Array map', async () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation, 'map-first');
    bindTrustedSlot(service, navigation, 'map-second');
    const inputs = [
      {
        intentId: 'poison-map-first',
        navigationGeneration: navigation.generation,
        operation: 'refresh' as const,
        registeredSlotId: 'map-first',
        requestClass: 'primary',
      },
      {
        intentId: 'poison-map-second',
        navigationGeneration: navigation.generation,
        operation: 'refresh' as const,
        registeredSlotId: 'map-second',
        requestClass: 'primary',
      },
    ];
    const originalMap = Array.prototype.map;
    Array.prototype.map = function <Value, Result>(
      this: Value[],
      callback: (value: Value, index: number, array: Value[]) => Result,
      thisArgument?: unknown
    ): Result[] {
      let targeted = false;
      for (let index = 0; index < this.length; index += 1) {
        const value = this[index] as { intentId?: unknown } | undefined;
        if (value?.intentId === 'poison-map-first') targeted = true;
      }
      if (targeted) {
        Reflect.apply(callback, thisArgument, [this[0], 0, this]);
        throw new Error('poisoned map after partial admission');
      }
      return Reflect.apply(originalMap, this, [callback, thisArgument]) as Result[];
    };
    let handles: readonly ReturnType<SlotService['request']>[] | undefined;
    let escaped: unknown;
    try {
      handles = service.requestBatch(inputs);
    } catch (error) {
      escaped = error;
    } finally {
      Array.prototype.map = originalMap;
    }

    expect(escaped).toBeUndefined();
    expect(handles).toHaveLength(2);
    for (const handle of handles ?? []) handle.dispose();
    await expect(Promise.all((handles ?? []).map(({ result }) => result))).resolves.toEqual([
      { reason: 'superseded', status: 'cancelled' },
      { reason: 'superseded', status: 'cancelled' },
    ]);
    await Promise.resolve();
    expect(harness.refresh).not.toHaveBeenCalled();
    expect(service.snapshotForTest().intents).toBe(0);
  });

  it('does not leak post-admission intents through a poisoned Array iterator', async () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation, 'iterator-first');
    bindTrustedSlot(service, navigation, 'iterator-second');
    const inputs = [
      {
        intentId: 'poison-iterator-first',
        navigationGeneration: navigation.generation,
        operation: 'refresh' as const,
        registeredSlotId: 'iterator-first',
        requestClass: 'primary',
      },
      {
        intentId: 'poison-iterator-second',
        navigationGeneration: navigation.generation,
        operation: 'refresh' as const,
        registeredSlotId: 'iterator-second',
        requestClass: 'primary',
      },
    ];
    const originalIterator = Array.prototype[Symbol.iterator];
    Array.prototype[Symbol.iterator] = function (): ArrayIterator<unknown> {
      for (let index = 0; index < this.length; index += 1) {
        const value = this[index] as { intentId?: unknown } | undefined;
        if (value?.intentId === 'poison-iterator-first') {
          throw new Error('poisoned iterator after admission');
        }
      }
      return Reflect.apply(originalIterator, this, []) as ArrayIterator<unknown>;
    };
    let handles: readonly ReturnType<SlotService['request']>[] | undefined;
    let escaped: unknown;
    try {
      handles = service.requestBatch(inputs);
    } catch (error) {
      escaped = error;
    } finally {
      Array.prototype[Symbol.iterator] = originalIterator;
    }

    expect(escaped).toBeUndefined();
    expect(handles).toHaveLength(2);
    for (let index = 0; index < (handles?.length ?? 0); index += 1) {
      handles?.[index]?.dispose();
    }
    await expect(Promise.all((handles ?? []).map(({ result }) => result))).resolves.toEqual([
      { reason: 'superseded', status: 'cancelled' },
      { reason: 'superseded', status: 'cancelled' },
    ]);
    await Promise.resolve();
    expect(harness.refresh).not.toHaveBeenCalled();
    expect(service.snapshotForTest().intents).toBe(0);
  });

  it('rolls back every admitted batch handle when a later request unexpectedly throws', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    let poison = false;
    let batchChecks = 0;
    const owner = {
      generation: {},
      isCurrent: () => {
        if (!poison) return true;
        batchChecks += 1;
        if (batchChecks === 4) throw new Error('second request admission failed');
        return true;
      },
      onDispose: vi.fn(),
    } as unknown as NavigationSession;
    bindTrustedSlot(service, owner, 'rollback-first');
    bindTrustedSlot(service, owner, 'rollback-second');
    const inventory = service.snapshotForTest();
    poison = true;

    expect(
      service.requestBatch([
        {
          intentId: 'rollback-first',
          navigationGeneration: owner.generation,
          operation: 'refresh',
          registeredSlotId: 'rollback-first',
          requestClass: 'primary',
        },
        {
          intentId: 'rollback-second',
          navigationGeneration: owner.generation,
          operation: 'refresh',
          registeredSlotId: 'rollback-second',
          requestClass: 'primary',
        },
      ])
    ).toEqual([]);
    expect(service.snapshotForTest()).toEqual(inventory);
  });

  it('keeps publisher display intent publisher-owned and fails ambiguous overlap', async () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    expect(service.recordPublisherIntent(slot)).toBe(true);

    const request = service.request({
      intentId: 'intent',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });

    await expect(request.result).resolves.toEqual({
      reason: 'cycle_unattributable',
      status: 'failed',
    });
    service.handleGptEvent('slotRequested', { slot });
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'publisher',
      slot,
    });
    expect(harness.refresh).not.toHaveBeenCalled();
  });

  it('allows one queued replacement, supersedes its same-class predecessor, and rejects opposite overlap', async () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    const request = (intentId: string, requestClass: string) =>
      service.request({
        intentId,
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        requestClass,
        registeredSlotId: 'slot',
      });
    const active = request('active', 'primary');
    const replaced = request('queued-one', 'primary');
    const queued = request('queued-two', 'primary');
    await expect(replaced.result).resolves.toEqual({
      reason: 'superseded',
      status: 'cancelled',
    });
    const conflicting = request('queued-fallback', 'fallback');
    await expect(queued.result).resolves.toEqual({
      reason: 'cycle_unattributable',
      status: 'failed',
    });
    await expect(conflicting.result).resolves.toEqual({
      reason: 'cycle_unattributable',
      status: 'failed',
    });
    await expect(active.result).resolves.toEqual({
      reason: 'cycle_unattributable',
      status: 'failed',
    });
  });

  it('queues one same-class replacement behind an open trusted-server cycle', async () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    const first = service.request({
      intentId: 'first-primary',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await Promise.resolve();
    service.handleGptEvent('slotRequested', { slot });

    const second = service.request({
      intentId: 'second-primary',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    expect(second.status).toBe('queued');
    expect(harness.refresh).toHaveBeenCalledTimes(1);

    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'first-primary-response',
      slot,
    });
    await expect(first.result).resolves.toEqual({
      responseIdentifier: 'first-primary-response',
      status: 'rendered',
    });
    await Promise.resolve();
    expect(harness.refresh).toHaveBeenCalledTimes(2);

    service.handleGptEvent('slotRequested', { slot });
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: true,
      responseIdentifier: 'second-primary-response',
      slot,
    });
    await expect(second.result).resolves.toEqual({
      responseIdentifier: 'second-primary-response',
      status: 'empty',
    });
  });

  it('promotes a queued replacement when its active predecessor cancels before invocation', async () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    const first = service.request({
      intentId: 'cancelled-before-invocation',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    const second = service.request({
      intentId: 'promoted-after-cancellation',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    expect(second.status).toBe('queued');

    first.dispose();
    await expect(first.result).resolves.toEqual({
      reason: 'superseded',
      status: 'cancelled',
    });
    await Promise.resolve();
    await Promise.resolve();
    expect(harness.refresh).toHaveBeenCalledTimes(1);
    expect(second.status).toBe('active');

    service.handleGptEvent('slotRequested', { slot });
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'promoted-response',
      slot,
    });
    await expect(second.result).resolves.toEqual({
      responseIdentifier: 'promoted-response',
      status: 'rendered',
    });
    expect(service.snapshotForTest()).toMatchObject({ cycles: 0, intents: 0 });
  });

  it('fails active and queued TS work when publisher intent makes ownership ambiguous', async () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    const active = service.request({
      intentId: 'active',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    const queued = service.request({
      intentId: 'queued',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });

    expect(service.recordPublisherIntent(slot)).toBe(true);
    await expect(active.result).resolves.toMatchObject({ reason: 'cycle_unattributable' });
    await expect(queued.result).resolves.toMatchObject({ reason: 'cycle_unattributable' });
    service.handleGptEvent('slotRequested', { slot });
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'publisher',
      slot,
    });
    expect(service.snapshotForTest().intents).toBe(0);
  });

  it('disposes an operation that settled synchronously before its handle was published', async () => {
    const harness = createGptHarness({ synchronousRun: true });
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    harness.refresh.mockImplementation(() => {
      service.handleGptEvent('slotRequested', { slot });
      service.handleGptEvent('slotRenderEnded', {
        isEmpty: false,
        responseIdentifier: 'synchronous',
        slot,
      });
    });

    const request = service.request({
      intentId: 'synchronous',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await expect(request.result).resolves.toMatchObject({ status: 'rendered' });
    expect(
      harness.operationDisposals[harness.operationDisposals.length - 1]
    ).toHaveBeenCalledOnce();
  });

  it('safe-retires an invoked pre-cycle cancellation instead of clearing its only safety timer', async () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    const request = service.request({
      intentId: 'cancelled',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await Promise.resolve();
    request.dispose();
    await expect(request.result).resolves.toMatchObject({ reason: 'superseded' });
    await Promise.resolve();

    expect(harness.destroySlots).toHaveBeenCalledTimes(1);
    expect(harness.defineSlot).toHaveBeenCalledTimes(1);
  });

  it.each([
    [2_999, true],
    [3_001, false],
  ] as const)('arbitrates slotRequested at %i ms without timeout re-arm', async (at, wins) => {
    vi.useFakeTimers();
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    const request = service.request({
      intentId: `intent-${at}`,
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await Promise.resolve();

    if (at < 3_000) {
      await vi.advanceTimersByTimeAsync(at);
      service.handleGptEvent('slotRequested', { slot });
    } else {
      await vi.advanceTimersByTimeAsync(at);
      service.handleGptEvent('slotRequested', { slot });
    }

    if (wins) {
      service.handleGptEvent('slotRenderEnded', {
        isEmpty: false,
        responseIdentifier: `response-${at}`,
        slot,
      });
      await expect(request.result).resolves.toMatchObject({ status: 'rendered' });
    } else {
      await expect(request.result).resolves.toEqual({
        reason: 'gpt_request_timeout',
        status: 'failed',
      });
      service.handleGptEvent('slotRenderEnded', {
        isEmpty: false,
        responseIdentifier: `late-${at}`,
        slot,
      });
      expect(service.snapshotForTest().cycles).toBe(0);
    }
  });

  it.each(['event-first', 'timeout-first'] as const)(
    'arbitrates callback registration order at the exact 3,000 ms boundary: %s',
    async (order) => {
      vi.useFakeTimers();
      const harness = createGptHarness();
      const service = createSlotService({ googletag: harness.adapter });
      const navigation = createNavigation();
      const slot = bindTrustedSlot(service, navigation);
      if (order === 'event-first') {
        setTimeout(() => service.handleGptEvent('slotRequested', { slot }), 3_000);
      }
      const request = service.request({
        intentId: order,
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        requestClass: 'primary',
        registeredSlotId: 'slot',
      });
      await Promise.resolve();
      if (order === 'timeout-first') {
        setTimeout(() => service.handleGptEvent('slotRequested', { slot }), 3_000);
      }
      await vi.advanceTimersByTimeAsync(3_000);

      if (order === 'event-first') {
        service.handleGptEvent('slotRenderEnded', {
          isEmpty: false,
          responseIdentifier: order,
          slot,
        });
        await expect(request.result).resolves.toMatchObject({ status: 'rendered' });
      } else {
        await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
      }
    }
  );

  it.each([
    [9_999, true],
    [10_001, false],
  ] as const)('arbitrates slotRenderEnded at %i ms from invocation', async (at, wins) => {
    vi.useFakeTimers();
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    const request = service.request({
      intentId: `intent-${at}`,
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await Promise.resolve();
    service.handleGptEvent('slotRequested', { slot });

    if (at < 10_000) {
      await vi.advanceTimersByTimeAsync(at);
      service.handleGptEvent('slotRenderEnded', {
        isEmpty: false,
        responseIdentifier: `response-${at}`,
        slot,
      });
    } else {
      await vi.advanceTimersByTimeAsync(at);
    }

    await expect(request.result).resolves.toEqual(
      wins
        ? { responseIdentifier: `response-${at}`, status: 'rendered' }
        : { reason: 'gpt_completion_timeout', status: 'failed' }
    );
  });

  it.each(['event-first', 'timeout-first'] as const)(
    'arbitrates callback registration order at the exact 10,000 ms boundary: %s',
    async (order) => {
      vi.useFakeTimers();
      const harness = createGptHarness();
      const service = createSlotService({ googletag: harness.adapter });
      const navigation = createNavigation();
      const slot = bindTrustedSlot(service, navigation);
      const request = service.request({
        intentId: `completion-${order}`,
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        requestClass: 'primary',
        registeredSlotId: 'slot',
      });
      await Promise.resolve();
      if (order === 'event-first') {
        setTimeout(
          () =>
            service.handleGptEvent('slotRenderEnded', {
              isEmpty: false,
              responseIdentifier: order,
              slot,
            }),
          10_000
        );
      }
      service.handleGptEvent('slotRequested', { slot });
      if (order === 'timeout-first') {
        setTimeout(
          () =>
            service.handleGptEvent('slotRenderEnded', {
              isEmpty: false,
              responseIdentifier: order,
              slot,
            }),
          10_000
        );
      }
      await vi.advanceTimersByTimeAsync(10_000);

      await expect(request.result).resolves.toMatchObject(
        order === 'event-first' ? { status: 'rendered' } : { reason: 'gpt_completion_timeout' }
      );
    }
  );

  it('deduplicates a response identifier without completing a replacement cycle', async () => {
    vi.useFakeTimers();
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    const first = service.request({
      intentId: 'first',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await Promise.resolve();
    service.handleGptEvent('slotRequested', { slot });
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'duplicate',
      slot,
    });
    await expect(first.result).resolves.toMatchObject({ status: 'rendered' });

    const second = service.request({
      intentId: 'second',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await Promise.resolve();
    service.handleGptEvent('slotRequested', { slot });
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'duplicate',
      slot,
    });
    await vi.advanceTimersByTimeAsync(10_000);
    await expect(second.result).resolves.toEqual({
      reason: 'gpt_completion_timeout',
      status: 'failed',
    });
  });

  it('recovers a completion timeout through the exact destroy/redefine transaction', async () => {
    vi.useFakeTimers();
    const harness = createGptHarness();
    const disposeCommittedArtifact = vi.fn();
    const service = createSlotService({
      disposeCommittedArtifact,
      googletag: harness.adapter,
    });
    const navigation = createNavigation();
    const oldSlot = bindTrustedSlot(service, navigation);
    const first = service.request({
      intentId: 'completion-timeout',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await Promise.resolve();
    service.handleGptEvent('slotRequested', { slot: oldSlot });
    await vi.advanceTimersByTimeAsync(10_000);
    await expect(first.result).resolves.toMatchObject({ reason: 'gpt_completion_timeout' });
    await Promise.resolve();
    const replacement = harness.defineSlot.mock.results[0]?.value;
    if (typeof replacement !== 'object' || replacement === null) {
      throw new Error('Expected completion-timeout replacement');
    }
    expect(harness.destroySlots).toHaveBeenCalledExactlyOnceWith([oldSlot]);
    expect(disposeCommittedArtifact).toHaveBeenCalledExactlyOnceWith(navigation.generation, 'slot');

    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'late-completion',
      slot: oldSlot,
    });
    const recovered = service.request({
      intentId: 'recovered',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await Promise.resolve();
    expect(recovered.status).toBe('active');
    expect(harness.refresh).toHaveBeenLastCalledWith(
      [replacement],
      Object.freeze({ changeCorrelator: false })
    );
    service.handleGptEvent('slotRequested', { slot: replacement });
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'replacement-completion',
      slot: replacement,
    });
    await expect(recovered.result).resolves.toEqual({
      responseIdentifier: 'replacement-completion',
      status: 'rendered',
    });
  });

  it('never releases publisher request-timeout quarantine from later GPT events', async () => {
    vi.useFakeTimers();
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const slot = { publisher: true };
    expect(service.register(navigation, [serverRegistration('slot')])).toMatchObject({ ok: true });
    expect(
      service.adoptGptSlot(navigation.generation, 'slot', {
        ownership: 'publisher',
        slot,
      })
    ).toEqual({ ok: true });
    const timedOut = service.request({
      intentId: 'publisher-timeout',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(3_000);
    await expect(timedOut.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
    service.handleGptEvent('slotRequested', { slot });
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'unattributable-late',
      slot,
    });
    const later = service.request({
      intentId: 'publisher-later',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await expect(later.result).resolves.toEqual({
      reason: 'gpt_request_failed',
      status: 'failed',
    });
  });

  it.each(['throw', 'false'] as const)(
    'keeps one retired object and quarantines failed request-timeout recovery: %s',
    async (failure) => {
      vi.useFakeTimers();
      const harness = createGptHarness();
      if (failure === 'throw') {
        harness.destroySlots.mockImplementation(() => {
          throw new Error('destroy failed');
        });
      } else {
        harness.destroySlots.mockReturnValue(false);
      }
      const service = createSlotService({ googletag: harness.adapter });
      const navigation = createNavigation();
      bindTrustedSlot(service, navigation);
      const timedOut = service.request({
        intentId: 'timed-out',
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        requestClass: 'primary',
        registeredSlotId: 'slot',
      });
      await Promise.resolve();
      await vi.advanceTimersByTimeAsync(3_000);
      await expect(timedOut.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
      await Promise.resolve();

      const later = service.request({
        intentId: 'later',
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        requestClass: 'primary',
        registeredSlotId: 'slot',
      });
      await expect(later.result).resolves.toEqual({
        reason: 'gpt_request_failed',
        status: 'failed',
      });
      expect(harness.defineSlot).not.toHaveBeenCalled();
      expect(service.snapshotForTest().physicalSlots).toBe(1);
    }
  );

  it('binds one successful request-timeout replacement and ignores events from the retired object', async () => {
    vi.useFakeTimers();
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const oldSlot = bindTrustedSlot(service, navigation);
    const timedOut = service.request({
      intentId: 'timed-out',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(3_000);
    await expect(timedOut.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
    await Promise.resolve();
    const replacement = harness.defineSlot.mock.results[0]?.value;
    if (typeof replacement !== 'object' || replacement === null) {
      throw new Error('Expected a replacement slot');
    }

    service.handleGptEvent('slotRequested', { slot: oldSlot });
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'retired-old',
      slot: oldSlot,
    });
    const later = service.request({
      intentId: 'later',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await Promise.resolve();
    expect(harness.refresh).toHaveBeenLastCalledWith(
      [replacement],
      Object.freeze({ changeCorrelator: false })
    );
    service.handleGptEvent('slotRequested', { slot: replacement });
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'replacement',
      slot: replacement,
    });
    await expect(later.result).resolves.toMatchObject({ status: 'rendered' });
  });

  it('destroys a replacement created after generation became stale and never binds it', async () => {
    vi.useFakeTimers();
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    harness.defineSlot.mockImplementation((_path, _sizes, elementId) => {
      const replacement = { elementId, replacement: true };
      navigation.dispose();
      return replacement;
    });
    const request = service.request({
      intentId: 'stale',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(3_000);
    await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
    await Promise.resolve();

    expect(harness.destroySlots).toHaveBeenCalledTimes(2);
    expect(service.resolveRegisteredSlot('slot')).toBeUndefined();
  });

  it('keeps publisher-owned navigation quarantine until its exact completion', async () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const slot = { publisher: true };
    expect(service.register(navigation, [serverRegistration('slot')])).toMatchObject({ ok: true });
    expect(
      service.adoptGptSlot(navigation.generation, 'slot', {
        ownership: 'publisher',
        slot,
      })
    ).toEqual({ ok: true });
    service.recordPublisherIntent(slot);
    service.handleGptEvent('slotRequested', { slot });
    navigation.dispose();
    expect(harness.destroySlots).not.toHaveBeenCalled();

    const next = createNavigation();
    expect(service.register(next, [serverRegistration('next')])).toMatchObject({ ok: true });
    expect(service.adoptGptSlot(next.generation, 'next', { ownership: 'publisher', slot })).toEqual(
      { ok: false, reason: 'slot_quarantined' }
    );
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'old-navigation',
      slot,
    });
    expect(service.adoptGptSlot(next.generation, 'next', { ownership: 'publisher', slot })).toEqual(
      { ok: true }
    );
  });

  it('blocks an active publisher placement across navigation until its completion drains', () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const registration = serverRegistration('slot', {
      adUnitCode: '/network/slot',
      domAliases: ['slot-div'],
    });
    const slot = { publisher: true };
    expect(service.register(navigation, [registration])).toMatchObject({ ok: true });
    expect(
      service.adoptGptSlot(navigation.generation, 'slot', { ownership: 'publisher', slot })
    ).toEqual({ ok: true });
    expect(service.recordPublisherIntent(slot)).toBe(true);
    service.handleGptEvent('slotRequested', { slot });
    navigation.dispose();

    const next = createNavigation();
    expect(service.register(next, [registration])).toEqual({
      ok: false,
      reason: 'slot_quarantined',
    });
    service.handleGptEvent('slotRenderEnded', { isEmpty: false, slot });
    expect(service.register(next, [registration])).toMatchObject({ ok: true });
  });

  it.each(['before', 'after'] as const)(
    'keeps an old completion inert %s replacement completion on the same DOM id',
    async (order) => {
      const harness = createGptHarness();
      const service = createSlotService({ googletag: harness.adapter });
      const { navigation, runtime } = createRuntimeWithNavigation();
      const oldSlot = bindTrustedSlot(service, navigation);
      const oldRequest = service.request({
        intentId: 'old',
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        requestClass: 'primary',
        registeredSlotId: 'slot',
      });
      await Promise.resolve();
      service.handleGptEvent('slotRequested', { slot: oldSlot });

      const replaced = runtime.replaceNavigation();
      if (!replaced.ok) throw new Error('Expected replacement navigation');
      await expect(oldRequest.result).resolves.toMatchObject({ reason: 'navigation_disposed' });
      const newSlot = bindTrustedSlot(service, replaced.value);
      const newRequest = service.request({
        intentId: 'new',
        navigationGeneration: replaced.value.generation,
        operation: 'refresh',
        requestClass: 'primary',
        registeredSlotId: 'slot',
      });
      await Promise.resolve();
      service.handleGptEvent('slotRequested', { slot: newSlot });
      const finishOld = () =>
        service.handleGptEvent('slotRenderEnded', {
          isEmpty: false,
          responseIdentifier: `old-${order}`,
          slot: oldSlot,
        });
      const finishNew = () =>
        service.handleGptEvent('slotRenderEnded', {
          isEmpty: false,
          responseIdentifier: `new-${order}`,
          slot: newSlot,
        });
      if (order === 'before') {
        finishOld();
        finishNew();
      } else {
        finishNew();
        finishOld();
      }

      await expect(newRequest.result).resolves.toEqual({
        responseIdentifier: `new-${order}`,
        status: 'rendered',
      });
      expect(service.snapshotForTest().cycles).toBe(0);
    }
  );

  it('releases a navigation-disposed TS physical slot with no late cycle bookkeeping', async () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);

    navigation.dispose();
    await Promise.resolve();
    await Promise.resolve();

    expect(harness.destroySlots).toHaveBeenCalledTimes(1);
    expect(service.snapshotForTest()).toMatchObject({ physicalSlots: 0, records: 0 });
  });
});

describe('Task 11 adversarial ownership review', () => {
  afterEach(() => vi.useRealTimers());

  it('accepts paired UTF-16 surrogates and rejects unpaired identities and aliases', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();

    expect(service.register(navigation, [serverRegistration('paired-😀')])).toMatchObject({
      ok: true,
    });

    for (const registration of [
      serverRegistration('broken-\ud800'),
      serverRegistration('broken-\udc00'),
      serverRegistration('slot', { adUnitCode: 'path-\ud800' }),
      serverRegistration('slot', { domAliases: ['alias-\udc00'] }),
    ]) {
      expect(service.register(navigation, [registration])).toEqual({
        ok: false,
        reason: 'invalid_slot_id',
      });
    }
  });

  it('re-adopts an idle publisher object without retaining its old navigation strongly', () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const { navigation, runtime } = createRuntimeWithNavigation();
    const slot = { publisher: true };
    expect(service.register(navigation, [serverRegistration('slot')])).toMatchObject({ ok: true });
    expect(
      service.adoptGptSlot(navigation.generation, 'slot', { ownership: 'publisher', slot })
    ).toEqual({ ok: true });

    const next = runtime.replaceNavigation();
    if (!next.ok) throw new Error('Expected replacement navigation');
    expect(service.snapshotForTest()).toMatchObject({ physicalSlots: 0, records: 0 });
    expect(service.register(next.value, [serverRegistration('slot')])).toMatchObject({ ok: true });
    expect(
      service.adoptGptSlot(next.value.generation, 'slot', { ownership: 'publisher', slot })
    ).toEqual({ ok: true });
    expect(service.snapshotForTest().physicalSlots).toBe(1);
  });

  it('rejects an existing GPT identity when the destination record already owns another object', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    const first = {};
    const second = {};
    expect(
      service.register(navigation, [serverRegistration('one'), serverRegistration('two')])
    ).toMatchObject({ ok: true });
    expect(
      service.adoptGptSlot(navigation.generation, 'one', { ownership: 'publisher', slot: first })
    ).toEqual({ ok: true });
    expect(
      service.adoptGptSlot(navigation.generation, 'two', { ownership: 'publisher', slot: second })
    ).toEqual({ ok: true });
    expect(
      service.adoptGptSlot(navigation.generation, 'two', { ownership: 'publisher', slot: first })
    ).toEqual({ ok: false, reason: 'gpt_object_collision' });
  });

  it('releases an exact publisher quarantine only through explicit publisher destruction', async () => {
    vi.useFakeTimers();
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const { navigation, runtime } = createRuntimeWithNavigation();
    const slot = { publisher: true };
    expect(service.register(navigation, [serverRegistration('slot')])).toMatchObject({ ok: true });
    expect(
      service.adoptGptSlot(navigation.generation, 'slot', { ownership: 'publisher', slot })
    ).toEqual({ ok: true });
    const request = service.request({
      intentId: 'publisher-timeout',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(3_000);
    await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });

    const next = runtime.replaceNavigation();
    if (!next.ok) throw new Error('Expected replacement navigation');
    expect(service.register(next.value, [serverRegistration('slot')])).toEqual({
      ok: false,
      reason: 'slot_quarantined',
    });
    expect(service.recordPublisherDestruction(slot)).toBe(true);
    expect(service.register(next.value, [serverRegistration('slot')])).toMatchObject({ ok: true });
  });

  it('quarantines every failed TS placement key and never retries its destroy on navigation', async () => {
    vi.useFakeTimers();
    const harness = createGptHarness();
    harness.destroySlots.mockReturnValue(false);
    const service = createSlotService({ googletag: harness.adapter });
    const { navigation, runtime } = createRuntimeWithNavigation();
    const slot = bindTrustedSlot(service, navigation);
    const request = service.request({
      intentId: 'timeout',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(3_000);
    await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
    expect(harness.destroySlots).toHaveBeenCalledTimes(1);

    const next = runtime.replaceNavigation();
    if (!next.ok) throw new Error('Expected replacement navigation');
    await Promise.resolve();
    expect(harness.destroySlots).toHaveBeenCalledTimes(1);
    for (const registration of [
      serverRegistration('slot'),
      serverRegistration('other-id', { adUnitCode: '/network/slot' }),
      serverRegistration('other-alias', { domAliases: ['slot-div'] }),
    ]) {
      expect(service.register(next.value, [registration])).toEqual({
        ok: false,
        reason: 'slot_quarantined',
      });
    }
    expect(service.recordPublisherDestruction(slot)).toBe(true);
    expect(service.register(next.value, [serverRegistration('slot')])).toMatchObject({ ok: true });
  });

  it('requires a usable replacement definition for trusted-server adoption', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    expect(service.register(navigation, [serverRegistration('slot')])).toMatchObject({ ok: true });
    expect(
      service.adoptGptSlot(navigation.generation, 'slot', {
        ownership: 'trusted_server',
        slot: {},
      })
    ).toEqual({ ok: false, reason: 'gpt_request_failed' });
  });

  it('reads a replacement definition once and owns an immutable placement snapshot', async () => {
    vi.useFakeTimers();
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    expect(
      service.register(navigation, [
        serverRegistration('slot', {
          adUnitCode: '/network/original',
          domAliases: ['original-div'],
        }),
      ])
    ).toMatchObject({ ok: true });
    let adUnitPath = '/network/original';
    let elementId = 'original-div';
    const sizes = [[300, 250]];
    const reads = { adUnitPath: 0, elementId: 0, sizes: 0 };
    const definition = {
      get adUnitPath() {
        reads.adUnitPath += 1;
        return adUnitPath;
      },
      get elementId() {
        reads.elementId += 1;
        return elementId;
      },
      get sizes() {
        reads.sizes += 1;
        return sizes;
      },
    };
    const slot = { original: true };
    expect(
      service.adoptGptSlot(navigation.generation, 'slot', {
        definition,
        ownership: 'trusted_server',
        slot,
      })
    ).toEqual({ ok: true });
    expect(reads).toEqual({ adUnitPath: 1, elementId: 1, sizes: 1 });

    adUnitPath = '/network/redirected';
    elementId = 'redirected-div';
    sizes[0] = [999, 999];
    const request = service.request({
      intentId: 'immutable-definition',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(3_000);
    await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
    await Promise.resolve();

    expect(reads).toEqual({ adUnitPath: 1, elementId: 1, sizes: 1 });
    expect(harness.defineSlot).toHaveBeenCalledWith(
      '/network/original',
      [[300, 250]],
      'original-div'
    );
  });

  it.each(['outer-array', 'inner-pair'] as const)(
    'contains a hostile replacement sizes graph without adoption mutation: %s',
    (failure) => {
      const service = createSlotService({ googletag: createGptHarness().adapter });
      const navigation = createNavigation();
      expect(service.register(navigation, [serverRegistration('slot')])).toMatchObject({
        ok: true,
      });
      const innerPair = new Proxy([300, 250], {
        get: (target, key, receiver) => {
          if (failure === 'inner-pair' && key === '0') throw new Error('hostile pair index');
          return Reflect.get(target, key, receiver);
        },
      });
      const sizes = new Proxy([innerPair], {
        get: (target, key, receiver) => {
          if (failure === 'outer-array' && key === 'length') {
            throw new Error('hostile sizes length');
          }
          return Reflect.get(target, key, receiver);
        },
      });
      const inventory = service.snapshotForTest();

      expect(
        service.adoptGptSlot(navigation.generation, 'slot', {
          definition: {
            adUnitPath: '/network/slot',
            elementId: 'slot-div',
            sizes,
          },
          ownership: 'trusted_server',
          slot: {},
        })
      ).toEqual({ ok: false, reason: 'gpt_request_failed' });
      expect(service.snapshotForTest()).toEqual(inventory);
      expect(service.resolveRegisteredSlot('slot')).toBeDefined();
    }
  );

  it('counts multiple publisher intents and preserves two publisher cycles', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    expect(service.recordPublisherIntent(slot)).toBe(true);
    expect(service.recordPublisherIntent(slot)).toBe(true);

    service.handleGptEvent('slotRequested', { slot });
    service.handleGptEvent('slotRenderEnded', { isEmpty: false, slot });
    service.handleGptEvent('slotRequested', { slot });
    expect(service.snapshotForTest().cycles).toBe(1);
    service.handleGptEvent('slotRenderEnded', { isEmpty: true, slot });
    expect(service.snapshotForTest().cycles).toBe(0);
  });

  it('bounds publisher intent accounting and fails closed on overflow', async () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    for (let index = 0; index < 64; index += 1) {
      expect(service.recordPublisherIntent(slot)).toBe(true);
    }
    expect(service.recordPublisherIntent(slot)).toBe(false);

    const blocked = service.request({
      intentId: 'publisher-overflow',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await expect(blocked.result).resolves.toEqual({
      reason: 'gpt_request_failed',
      status: 'failed',
    });
  });

  it('fails and conservatively drains a TS cycle overlapped by publisher intent', async () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    const active = service.request({
      intentId: 'active',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    service.handleGptEvent('slotRequested', { slot });

    expect(service.recordPublisherIntent(slot)).toBe(true);
    await expect(active.result).resolves.toMatchObject({ reason: 'cycle_unattributable' });
    const blocked = service.request({
      intentId: 'blocked',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await expect(blocked.result).resolves.toMatchObject({ reason: 'cycle_unattributable' });
    service.handleGptEvent('slotRenderEnded', { isEmpty: false, slot });
    service.handleGptEvent('slotRequested', { slot });
    expect(service.snapshotForTest().cycles).toBe(1);
  });

  it('rejects the first opposite-class queued request with the active intent', async () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    const active = service.request({
      intentId: 'active',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    const opposite = service.request({
      intentId: 'opposite',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'fallback',
    });

    await expect(active.result).resolves.toMatchObject({ reason: 'cycle_unattributable' });
    await expect(opposite.result).resolves.toMatchObject({ reason: 'cycle_unattributable' });
  });

  it('quarantines a synchronous requested cycle when the external invocation then throws', async () => {
    const harness = createGptHarness({ synchronousRun: true });
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    harness.refresh.mockImplementation(() => {
      service.handleGptEvent('slotRequested', { slot });
      throw new Error('after-side-effect');
    });
    const request = service.request({
      intentId: 'partial',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_failed' });
    const blocked = service.request({
      intentId: 'blocked',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await expect(blocked.result).resolves.toMatchObject({ reason: 'slot_quarantined' });
    service.handleGptEvent('slotRenderEnded', { isEmpty: false, slot });
  });

  it('keeps a shared synchronous SRA operation alive for an unfinished sibling', async () => {
    const harness = createGptHarness({ synchronousRun: true });
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const first = bindTrustedSlot(service, navigation, 'first');
    const second = bindTrustedSlot(service, navigation, 'second');
    harness.refresh.mockImplementation(() => {
      service.handleGptEvent('slotRequested', { slot: first });
      service.handleGptEvent('slotRenderEnded', { isEmpty: false, slot: first });
      service.handleGptEvent('slotRequested', { slot: second });
    });
    const requests = service.requestBatch([
      {
        intentId: 'first',
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        registeredSlotId: 'first',
        requestClass: 'primary',
      },
      {
        intentId: 'second',
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        registeredSlotId: 'second',
        requestClass: 'primary',
      },
    ]);
    await expect(requests[0]?.result).resolves.toMatchObject({ status: 'rendered' });
    expect(requests[1]?.status).toBe('active');
    expect(
      harness.operationDisposals[harness.operationDisposals.length - 1]
    ).not.toHaveBeenCalled();
    service.handleGptEvent('slotRenderEnded', { isEmpty: true, slot: second });
    await expect(requests[1]?.result).resolves.toMatchObject({ status: 'empty' });
  });

  it('does not invoke an SRA batch after its subscription continuation is disposed', async () => {
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation, 'deferred-first');
    bindTrustedSlot(service, navigation, 'deferred-second');
    const requests = service.requestBatch([
      {
        intentId: 'deferred-first',
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        registeredSlotId: 'deferred-first',
        requestClass: 'primary',
      },
      {
        intentId: 'deferred-second',
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        registeredSlotId: 'deferred-second',
        requestClass: 'primary',
      },
    ]);
    navigation.dispose();

    await expect(Promise.all(requests.map(({ result }) => result))).resolves.toEqual([
      { reason: 'navigation_disposed', status: 'cancelled' },
      { reason: 'navigation_disposed', status: 'cancelled' },
    ]);
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
    expect(harness.refresh).not.toHaveBeenCalled();
  });

  it('enforces delayed-handler deadlines from invocation with a monotonic injected clock', async () => {
    vi.useFakeTimers();
    let current = 100;
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter, now: () => current });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    const request = service.request({
      intentId: 'delayed',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    current = 3_101;
    service.handleGptEvent('slotRequested', { slot });
    await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
  });

  it('does not let a timer fire before the injected clock reaches its deadline', async () => {
    vi.useFakeTimers();
    let current = 0;
    const service = createSlotService({
      googletag: createGptHarness().adapter,
      now: () => current,
    });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    const request = service.request({
      intentId: 'lagged-clock',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    current = 2_999;
    await vi.advanceTimersByTimeAsync(3_000);
    expect(request.status).toBe('active');
    current = 3_000;
    await vi.advanceTimersByTimeAsync(1);
    await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
  });

  it('fails closed on malformed completion truth instead of rendering it', async () => {
    vi.useFakeTimers();
    const malformedEvents = [
      {},
      { isEmpty: 'false' },
      Object.defineProperty({}, 'isEmpty', { get: () => false }),
    ];
    for (let index = 0; index < malformedEvents.length; index += 1) {
      const service = createSlotService({ googletag: createGptHarness().adapter });
      const navigation = createNavigation();
      const slot = bindTrustedSlot(service, navigation, `slot-${index}`);
      const request = service.request({
        intentId: `malformed-${index}`,
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        registeredSlotId: `slot-${index}`,
        requestClass: 'primary',
      });
      await Promise.resolve();
      service.handleGptEvent('slotRequested', { slot });
      const event = { slot };
      const malformed = malformedEvents[index];
      const descriptor = malformed
        ? Object.getOwnPropertyDescriptor(malformed, 'isEmpty')
        : undefined;
      if (descriptor) Object.defineProperty(event, 'isEmpty', descriptor);
      service.handleGptEvent('slotRenderEnded', event);
      expect(request.status).toBe('active');
      await vi.advanceTimersByTimeAsync(10_000);
      await expect(request.result).resolves.toMatchObject({ reason: 'gpt_completion_timeout' });
    }
  });

  it('enforces the completion deadline in the handler when timer delivery is blocked', async () => {
    vi.useFakeTimers();
    let current = 0;
    const harness = createGptHarness();
    const service = createSlotService({
      googletag: harness.adapter,
      now: () => current,
    });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    const request = service.request({
      intentId: 'blocked-completion-timer',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    current = 100;
    service.handleGptEvent('slotRequested', { slot });
    current = 10_001;
    service.handleGptEvent('slotRenderEnded', { isEmpty: false, slot });

    await expect(request.result).resolves.toMatchObject({ reason: 'gpt_completion_timeout' });
    expect(service.snapshotForTest().cycles).toBe(0);
    const replacement = harness.defineSlot.mock.results[0]?.value;
    if (typeof replacement !== 'object' || replacement === null) {
      throw new Error('Expected handler-enforced timeout replacement');
    }

    const next = service.request({
      intentId: 'after-late-exact-completion',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    expect(harness.refresh).toHaveBeenCalledTimes(2);
    service.handleGptEvent('slotRequested', { slot: replacement });
    service.handleGptEvent('slotRenderEnded', { isEmpty: false, slot: replacement });
    await expect(next.result).resolves.toMatchObject({ status: 'rendered' });
  });

  it('fails active and queued work when publisher intent overlaps the opened TS cycle', async () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    const active = service.request({
      intentId: 'active',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    const queued = service.request({
      intentId: 'queued',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    service.handleGptEvent('slotRequested', { slot });

    expect(service.recordPublisherIntent(slot)).toBe(true);
    await expect(active.result).resolves.toMatchObject({ reason: 'cycle_unattributable' });
    await expect(queued.result).resolves.toMatchObject({ reason: 'cycle_unattributable' });
  });

  it('keeps promoted listeners across an async command that emits synchronously and then throws', async () => {
    const commands: Array<() => void> = [];
    const listeners = new Map<string, Set<(event: unknown) => void>>();
    const pubads = {
      addEventListener: (type: string, listener: (event: unknown) => void) => {
        const current = listeners.get(type) ?? new Set();
        current.add(listener);
        listeners.set(type, current);
      },
      getSlots: () => [slot],
      refresh: vi.fn(() => {
        for (const listener of listeners.get('slotRequested') ?? []) listener({ slot });
        throw new Error('after synchronous event');
      }),
      removeEventListener: (type: string, listener: (event: unknown) => void) => {
        listeners.get(type)?.delete(listener);
      },
    };
    const adapter = createBrowserGoogletagAdapter({
      googletag: {
        apiReady: true,
        cmd: { push: (command: () => void) => commands.push(command) },
        display: vi.fn(),
        pubads: () => pubads,
        pubadsReady: true,
      },
    });
    const service = createSlotService({ googletag: adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    const request = service.request({
      intentId: 'async-partial',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    commands.shift()?.();
    await Promise.resolve();
    await Promise.resolve();
    commands.shift()?.();

    await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_failed' });
    expect(listeners.get('slotRequested')?.size).toBe(1);
    expect(listeners.get('slotRenderEnded')?.size).toBe(1);
    for (const listener of listeners.get('slotRenderEnded') ?? []) {
      listener({ isEmpty: false, slot });
    }
  });

  it('quarantines every synchronously opened SRA cycle when shared refresh throws', async () => {
    const harness = createGptHarness({ synchronousRun: true });
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const first = bindTrustedSlot(service, navigation, 'first-partial');
    const second = bindTrustedSlot(service, navigation, 'second-partial');
    harness.refresh.mockImplementation(() => {
      service.handleGptEvent('slotRequested', { slot: first });
      service.handleGptEvent('slotRequested', { slot: second });
      throw new Error('shared refresh failed');
    });
    const requests = service.requestBatch([
      {
        intentId: 'first-partial',
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        registeredSlotId: 'first-partial',
        requestClass: 'primary',
      },
      {
        intentId: 'second-partial',
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        registeredSlotId: 'second-partial',
        requestClass: 'primary',
      },
    ]);

    await expect(Promise.all(requests.map(({ result }) => result))).resolves.toEqual([
      { reason: 'gpt_request_failed', status: 'failed' },
      { reason: 'gpt_request_failed', status: 'failed' },
    ]);
    expect(service.snapshotForTest().cycles).toBe(2);
  });

  it('tracks an exact orphan candidate until publisher destruction releases its placement', async () => {
    vi.useFakeTimers();
    const orphan = { orphan: true };
    const harness = createGptHarness({ orphanOnReplace: orphan });
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    const request = service.request({
      intentId: 'orphan',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(3_000);
    await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
    await Promise.resolve();
    expect(service.recordPublisherDestruction(orphan)).toBe(true);
    expect(
      service.adoptGptSlot(navigation.generation, 'slot', {
        definition: {
          adUnitPath: '/network/slot',
          elementId: 'slot-div',
          sizes: [[300, 250]],
        },
        ownership: 'trusted_server',
        slot: { replacementAfterOrphan: true },
      })
    ).toEqual({ ok: true });
  });

  it('retains a reused old identity when rejecting it cannot destroy the candidate', async () => {
    vi.useFakeTimers();
    const harness = createGptHarness({ returnOldOnReplace: true });
    harness.destroySlots.mockReturnValueOnce(true).mockReturnValueOnce(false);
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const oldSlot = bindTrustedSlot(service, navigation);
    const request = service.request({
      intentId: 'reused-old-orphan',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(3_000);
    await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
    await Promise.resolve();

    expect(
      service.adoptGptSlot(navigation.generation, 'slot', {
        definition: {
          adUnitPath: '/network/slot',
          elementId: 'slot-div',
          sizes: [[300, 250]],
        },
        ownership: 'trusted_server',
        slot: { blocked: true },
      })
    ).toEqual({ ok: false, reason: 'slot_quarantined' });
    expect(service.recordPublisherDestruction(oldSlot)).toBe(true);
  });

  it.each([true, false])(
    'never cleans or republishes a replacement candidate owned by another record: cleanup=%s',
    async (candidateCleanupWouldSucceed) => {
      vi.useFakeTimers();
      const harness = createReplacementHarness();
      harness.destroySlots
        .mockReturnValueOnce(true)
        .mockReturnValueOnce(candidateCleanupWouldSucceed);
      const service = createSlotService({ googletag: harness.adapter });
      const navigation = createNavigation();
      const firstDefinition = Object.freeze({
        adUnitPath: '/network/first',
        elementId: 'first-div',
        sizes: Object.freeze([[300, 250]]),
      });
      const secondDefinition = Object.freeze({
        adUnitPath: '/network/second',
        elementId: 'second-div',
        sizes: Object.freeze([[300, 250]]),
      });
      const oldSlot = { old: true };
      expect(
        service.register(navigation, [
          serverRegistration('first', {
            adUnitCode: firstDefinition.adUnitPath,
            domAliases: [firstDefinition.elementId],
          }),
          serverRegistration('second', {
            adUnitCode: secondDefinition.adUnitPath,
            domAliases: [secondDefinition.elementId],
          }),
        ])
      ).toMatchObject({ ok: true });
      expect(
        service.adoptGptSlot(navigation.generation, 'first', {
          definition: firstDefinition,
          ownership: 'trusted_server',
          slot: oldSlot,
        })
      ).toEqual({ ok: true });
      expect(
        service.adoptGptSlot(navigation.generation, 'second', {
          definition: secondDefinition,
          ownership: 'trusted_server',
          slot: harness.replacement,
        })
      ).toEqual({ ok: true });
      const request = service.request({
        intentId: `collision-${String(candidateCleanupWouldSucceed)}`,
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        registeredSlotId: 'first',
        requestClass: 'primary',
      });
      await Promise.resolve();
      await vi.advanceTimersByTimeAsync(3_000);
      await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
      await Promise.resolve();

      expect(harness.destroySlots).toHaveBeenCalledOnce();
      expect(harness.replacement.addService).not.toHaveBeenCalled();
      expect(
        service.adoptGptSlot(navigation.generation, 'second', {
          definition: secondDefinition,
          ownership: 'trusted_server',
          slot: harness.replacement,
        })
      ).toEqual({ ok: true });
      const blocked = service.request({
        intentId: 'original-remains-quarantined',
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        registeredSlotId: 'first',
        requestClass: 'primary',
      });
      await expect(blocked.result).resolves.toMatchObject({ reason: 'gpt_request_failed' });
    }
  );

  it('leaves a clean define failure unbound and immediately re-adoptable', async () => {
    vi.useFakeTimers();
    const harness = createGptHarness();
    harness.defineSlot.mockReturnValue(undefined);
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    const request = service.request({
      intentId: 'define-failure',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(3_000);
    await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
    await Promise.resolve();

    expect(
      service.adoptGptSlot(navigation.generation, 'slot', {
        definition: {
          adUnitPath: '/network/slot',
          elementId: 'slot-div',
          sizes: [[300, 250]],
        },
        ownership: 'trusted_server',
        slot: { retry: true },
      })
    ).toEqual({ ok: true });
  });

  it('deletes a stale destroyed identity so a later navigation may adopt it', async () => {
    vi.useFakeTimers();
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const { navigation, runtime } = createRuntimeWithNavigation();
    const oldSlot = bindTrustedSlot(service, navigation);
    harness.defineSlot.mockImplementation((_path, _sizes, elementId) => {
      const candidate = { elementId };
      runtime.replaceNavigation();
      return candidate;
    });
    const request = service.request({
      intentId: 'stale-destroyed',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      registeredSlotId: 'slot',
      requestClass: 'primary',
    });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(3_000);
    await expect(request.result).resolves.toMatchObject({ reason: 'gpt_request_timeout' });
    await Promise.resolve();
    const next = runtime.currentNavigation;
    if (!next) throw new Error('Expected replacement navigation');
    expect(service.register(next, [serverRegistration('slot')])).toMatchObject({ ok: true });
    expect(
      service.adoptGptSlot(next.generation, 'slot', { ownership: 'publisher', slot: oldSlot })
    ).toEqual({ ok: true });
  });

  it.each(['single', 'batch'] as const)(
    'rolls back provisional service subscription admission after %s preflight rejection',
    async (kind) => {
      const harness = createGptHarness({ synchronousRun: true });
      const subscribe = vi.fn((_type: string, _listener: (event: unknown) => void) => vi.fn());
      const facade = Object.freeze({ ...harness.facade, subscribe });
      let rejectNext = true;
      const adapter: GoogletagAdapter = Object.freeze({
        bindingStatus: () => 'present',
        dispose: vi.fn(),
        notifyReady: vi.fn(),
        observePublisherCalls: () => vi.fn(),
        run: <T>(command: (gpt: Readonly<GoogletagFacade>) => T) => {
          let value: T;
          try {
            value = command(facade);
          } catch (error) {
            return Object.freeze({
              status: 'present' as const,
              result: Promise.reject(error),
              dispose: vi.fn(),
            });
          }
          const result = rejectNext
            ? Promise.reject(new Error('post-command rejection'))
            : Promise.resolve(value);
          rejectNext = false;
          return Object.freeze({ status: 'present' as const, result, dispose: vi.fn() });
        },
      });
      const service = createSlotService({ googletag: adapter });
      const navigation = createNavigation();
      bindTrustedSlot(service, navigation);
      const input = {
        intentId: 'preflight',
        navigationGeneration: navigation.generation,
        operation: 'refresh' as const,
        registeredSlotId: 'slot',
        requestClass: 'primary',
      };
      const failed = kind === 'single' ? [service.request(input)] : service.requestBatch([input]);
      await expect(failed[0]?.result).resolves.toMatchObject({ reason: 'gpt_request_failed' });
      const retried = service.request({ ...input, intentId: 'retry' });
      await Promise.resolve();

      expect(subscribe).toHaveBeenCalledTimes(4);
      retried.dispose();
    }
  );

  it('fails closed after bounded placement quarantine storage saturates', () => {
    const harness = createGptHarness();
    harness.destroySlots.mockReturnValue(false);
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    for (let recordIndex = 0; recordIndex < 9; recordIndex += 1) {
      const id = `saturated-${recordIndex}`;
      const aliases = Array.from({ length: 256 }, (_, aliasIndex) => `${id}-alias-${aliasIndex}`);
      expect(
        service.register(navigation, [
          serverRegistration(id, { adUnitCode: `/network/${id}`, domAliases: aliases }),
        ])
      ).toMatchObject({ ok: true });
      expect(
        service.adoptGptSlot(navigation.generation, id, {
          definition: {
            adUnitPath: `/network/${id}`,
            elementId: aliases[0] ?? `${id}-div`,
            sizes: [[300, 250]],
          },
          ownership: 'trusted_server',
          slot: { id },
        })
      ).toEqual({ ok: true });
    }
    navigation.dispose();
    const next = createNavigation();
    expect(service.register(next, [serverRegistration('unrelated')])).toEqual({
      ok: false,
      reason: 'slot_quarantined',
    });
  });

  it('clears saturated placement quarantine only after every saturated owner releases once', () => {
    const harness = createGptHarness();
    harness.destroySlots.mockReturnValue(false);
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const oldSlots: object[] = [];
    for (let recordIndex = 0; recordIndex < 9; recordIndex += 1) {
      const id = `recover-saturated-${recordIndex}`;
      const aliases = Array.from({ length: 256 }, (_, aliasIndex) => `${id}-${aliasIndex}`);
      const slot = { id };
      oldSlots[oldSlots.length] = slot;
      expect(
        service.register(navigation, [
          serverRegistration(id, { adUnitCode: `/network/${id}`, domAliases: aliases }),
        ])
      ).toMatchObject({ ok: true });
      expect(
        service.adoptGptSlot(navigation.generation, id, {
          definition: {
            adUnitPath: `/network/${id}`,
            elementId: aliases[0] ?? `${id}-div`,
            sizes: [[300, 250]],
          },
          ownership: 'trusted_server',
          slot,
        })
      ).toEqual({ ok: true });
    }
    navigation.dispose();
    const next = createNavigation();
    expect(service.register(next, [serverRegistration('unrelated-after-saturation')])).toEqual({
      ok: false,
      reason: 'slot_quarantined',
    });

    for (let index = 0; index < oldSlots.length - 1; index += 1) {
      expect(service.recordPublisherDestruction(oldSlots[index] as object)).toBe(true);
    }
    expect(service.recordPublisherDestruction(oldSlots[7] as object)).toBe(false);
    expect(service.register(next, [serverRegistration('unrelated-after-saturation')])).toEqual({
      ok: false,
      reason: 'slot_quarantined',
    });

    expect(service.recordPublisherDestruction(oldSlots[8] as object)).toBe(true);
    expect(
      service.register(next, [serverRegistration('unrelated-after-saturation')])
    ).toMatchObject({
      ok: true,
    });
  });

  it.each(['throw-before', 'mutate-then-throw'] as const)(
    'releases only confirmed shared-key quarantine increments: %s',
    async (failure) => {
      const originalSet = Map.prototype.set;
      let poison = false;
      Map.prototype.set = function <Key, Value>(
        this: Map<Key, Value>,
        key: Key,
        value: Value
      ): Map<Key, Value> {
        const targeted = poison && key === ('ad-unit:/shared' as Key) && value === (2 as Value);
        if (targeted && failure === 'throw-before') throw new Error('failed before increment');
        const result = Reflect.apply(originalSet, this, [key, value]) as Map<Key, Value>;
        if (targeted) throw new Error('failed after increment');
        return result;
      };
      vi.resetModules();
      let fresh: typeof import('../../src/services/slots');
      try {
        fresh = await import('../../src/services/slots');
      } finally {
        Map.prototype.set = originalSet;
      }
      const harness = createGptHarness();
      harness.destroySlots.mockReturnValue(false);
      const service = fresh.createSlotService({ googletag: harness.adapter });
      const navigation = createNavigation();
      const firstSlot = { first: true };
      const secondSlot = { second: true };
      for (const [id, slot] of [
        ['first', firstSlot],
        ['second', secondSlot],
      ] as const) {
        expect(
          service.register(navigation, [
            serverRegistration(id, { adUnitCode: '/shared', domAliases: [`${id}-div`] }),
          ])
        ).toMatchObject({ ok: true });
        expect(
          service.adoptGptSlot(navigation.generation, id, {
            definition: {
              adUnitPath: `/network/${id}`,
              elementId: `${id}-div`,
              sizes: [[300, 250]],
            },
            ownership: 'trusted_server',
            slot,
          })
        ).toEqual({ ok: true });
      }
      poison = true;
      navigation.dispose();
      const next = createNavigation();

      expect(service.recordPublisherDestruction(secondSlot)).toBe(true);
      expect(service.recordPublisherDestruction(secondSlot)).toBe(false);
      expect(
        service.register(next, [serverRegistration('third', { adUnitCode: '/shared' })])
      ).toEqual({ ok: false, reason: 'slot_quarantined' });

      expect(service.recordPublisherDestruction(firstSlot)).toBe(true);
      expect(
        service.register(next, [serverRegistration('third', { adUnitCode: '/shared' })])
      ).toMatchObject({ ok: true });
    }
  );

  it('rolls back a Map publication whose captured set mutates and then throws', async () => {
    const originalSet = Map.prototype.set;
    let poison = false;
    Map.prototype.set = function <K, V>(this: Map<K, V>, key: K, value: V): Map<K, V> {
      const result = Reflect.apply(originalSet, this, [key, value]) as Map<K, V>;
      if (poison && key === 'mutate-then-throw-slot') throw new Error('mutated then threw');
      return result;
    };
    vi.resetModules();
    let fresh: typeof import('../../src/services/slots');
    try {
      fresh = await import('../../src/services/slots');
    } finally {
      Map.prototype.set = originalSet;
    }
    const service = fresh.createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    poison = true;
    expect(service.register(navigation, [serverRegistration('mutate-then-throw-slot')])).toEqual({
      ok: false,
      reason: 'stale_owner',
    });
    poison = false;
    expect(service.resolveRegisteredSlot('mutate-then-throw-slot')).toBeUndefined();
    expect(service.snapshotForTest().records).toBe(0);
  });

  it('uses captured iterator next intrinsics after publisher prototype poisoning', () => {
    const service = createSlotService({ googletag: createGptHarness().adapter });
    const navigation = createNavigation();
    bindTrustedSlot(service, navigation);
    const mapIteratorPrototype = Object.getPrototypeOf(new Map().values()) as {
      next: () => IteratorResult<unknown>;
    };
    const setIteratorPrototype = Object.getPrototypeOf(new Set().values()) as {
      next: () => IteratorResult<unknown>;
    };
    const mapNext = mapIteratorPrototype.next;
    const setNext = setIteratorPrototype.next;
    mapIteratorPrototype.next = () => {
      throw new Error('poisoned map iterator');
    };
    setIteratorPrototype.next = () => {
      throw new Error('poisoned set iterator');
    };
    try {
      expect(service.snapshotForTest()).toMatchObject({ physicalSlots: 1, records: 1 });
      expect(() => service.dispose()).not.toThrow();
    } finally {
      mapIteratorPrototype.next = mapNext;
      setIteratorPrototype.next = setNext;
    }
  });
});
