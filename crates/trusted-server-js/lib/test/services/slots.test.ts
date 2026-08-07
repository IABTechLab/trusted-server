import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  createBrowserGoogletagAdapter,
  type GoogletagAdapter,
  type GoogletagFacade,
  type GoogletagReplacementDefinition,
} from '../../src/adapters/googletag';
import { createTestNavigationIdentityIssuer } from '../../src/kernel/identity';
import { createRuntimeSession, type NavigationSession } from '../../src/kernel/sessions';
import {
  MAX_ACTIVE_SLOT_RECORDS,
  createSlotService,
  type GptSlotBinding,
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
    missingRefresh?: boolean;
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
  const facade: GoogletagFacade = Object.freeze({
    bindingToken: () => bindingToken,
    clearTargeting: vi.fn(),
    display,
    getTargeting: vi.fn(() => []),
    observeTargeting: () => vi.fn(),
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
      isCurrent: () => boolean
    ) => {
      if (!destroySlots([oldSlot])) return undefined;
      if (!definition || !isCurrent()) return undefined;
      const replacement = defineSlot(definition.adUnitPath, definition.sizes, definition.elementId);
      if (!replacement) return undefined;
      if (!isCurrent()) {
        destroySlots([replacement]);
        return undefined;
      }
      addService(replacement);
      if (!isCurrent()) {
        destroySlots([replacement]);
        return undefined;
      }
      return replacement;
    },
  });
  const adapter: GoogletagAdapter = Object.freeze({
    bindingStatus: () => 'present',
    dispose: vi.fn(),
    notifyReady: vi.fn(),
    run: <T>(command: (gpt: Readonly<GoogletagFacade>) => T) => {
      let disposed = false;
      const dispose = vi.fn(() => {
        disposed = true;
      });
      operationDisposals.push(dispose);
      let result: Promise<T>;
      if (options.synchronousRun) {
        try {
          result = Promise.resolve(command(facade));
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

describe('slot registry', () => {
  afterEach(() => vi.useRealTimers());

  it('accepts exact nonempty 256-byte ids and rejects empty, 257-byte, NUL, and controls', () => {
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
      `c1${String.fromCharCode(0x85)}`,
    ]) {
      expect(service.register(navigation, [serverRegistration(invalid)])).toEqual({
        ok: false,
        reason: 'invalid_slot_id',
      });
    }
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

  it.each(['throw', 'false', 'define'] as const)(
    'never publishes a second physical slot after %s failure',
    async (failure) => {
      const harness = createReplacementHarness();
      if (failure === 'throw') {
        harness.destroySlots.mockImplementation(() => {
          throw new Error('destroy failed');
        });
      } else if (failure === 'false') {
        harness.destroySlots.mockReturnValue(false);
      } else {
        harness.defineSlot.mockReturnValue(undefined);
      }
      const operation = harness.adapter.run((gpt) =>
        gpt.transactionalReplace({}, definition, () => true)
      );

      await expect(operation.result).rejects.toBeDefined();
      expect(harness.defineSlot).toHaveBeenCalledTimes(failure === 'define' ? 1 : 0);
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
        gpt.transactionalReplace({}, definition, () => {
          checks += 1;
          return checks < staleAt;
        })
      );

      await expect(operation.result).resolves.toBeUndefined();
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
      gpt.transactionalReplace({}, definition, () => {
        checks += 1;
        return checks < 2;
      })
    );

    await expect(operation.result).rejects.toBeDefined();
    expect(harness.destroySlots).toHaveBeenCalledTimes(2);
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
  it('retries after readiness timeout and never duplicates listeners on the recovered binding', async () => {
    vi.useFakeTimers();
    const target: { googletag?: unknown } = {};
    const adapter = createBrowserGoogletagAdapter(target);
    const service = createSlotService({ googletag: adapter });
    const missing = service.activate();
    await vi.advanceTimersByTimeAsync(10_000);
    await expect(missing.result).rejects.toMatchObject({ code: 'external_ready_timeout' });

    const ready = readyListenerBinding();
    target.googletag = ready.binding;
    await expect(service.activate().result).resolves.toBeUndefined();
    await expect(service.activate().result).resolves.toBeUndefined();

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
    await expect(service.activate().result).resolves.toBeUndefined();
    target.googletag = second.binding;
    await expect(service.activate().result).resolves.toBeUndefined();
    await expect(service.activate().result).resolves.toBeUndefined();

    expect(first.addEventListener).toHaveBeenCalledTimes(2);
    expect(second.addEventListener).toHaveBeenCalledTimes(2);
    service.dispose();
    expect(first.removeEventListener).toHaveBeenCalledTimes(2);
    expect(second.removeEventListener).toHaveBeenCalledTimes(2);
  });
});

describe('physical GPT cycles', () => {
  afterEach(() => vi.useRealTimers());

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
    active.dispose();
    await expect(active.result).resolves.toEqual({
      reason: 'superseded',
      status: 'cancelled',
    });
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
    expect(harness.operationDisposals[0]).toHaveBeenCalledOnce();
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

  it('keeps a completion-timeout cycle quarantined until its exact late completion drains', async () => {
    vi.useFakeTimers();
    const harness = createGptHarness();
    const service = createSlotService({ googletag: harness.adapter });
    const navigation = createNavigation();
    const slot = bindTrustedSlot(service, navigation);
    const first = service.request({
      intentId: 'completion-timeout',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await Promise.resolve();
    service.handleGptEvent('slotRequested', { slot });
    await vi.advanceTimersByTimeAsync(10_000);
    await expect(first.result).resolves.toMatchObject({ reason: 'gpt_completion_timeout' });
    const blocked = service.request({
      intentId: 'blocked',
      navigationGeneration: navigation.generation,
      operation: 'refresh',
      requestClass: 'primary',
      registeredSlotId: 'slot',
    });
    await expect(blocked.result).resolves.toMatchObject({ reason: 'slot_quarantined' });

    service.handleGptEvent('slotRequested', { slot });
    expect(service.snapshotForTest().cycles).toBe(1);
    service.handleGptEvent('slotRenderEnded', {
      isEmpty: false,
      responseIdentifier: 'late-completion',
      slot,
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
    recovered.dispose();
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

  it.each(['throw', 'false', 'define'] as const)(
    'keeps one retired object and quarantines failed request-timeout recovery: %s',
    async (failure) => {
      vi.useFakeTimers();
      const harness = createGptHarness();
      if (failure === 'throw') {
        harness.destroySlots.mockImplementation(() => {
          throw new Error('destroy failed');
        });
      } else if (failure === 'false') {
        harness.destroySlots.mockReturnValue(false);
      } else {
        harness.defineSlot.mockReturnValue(undefined);
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
      expect(harness.defineSlot).toHaveBeenCalledTimes(failure === 'define' ? 1 : 0);
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
