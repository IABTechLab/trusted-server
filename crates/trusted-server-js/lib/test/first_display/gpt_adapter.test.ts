import { describe, expect, it, vi } from 'vitest';
import { JSDOM } from 'jsdom';

import { createFirstDisplayGoogletagBatch } from '../../src/first_display/adapters/googletag';
import { enqueueFirstDisplayGamAttribution } from '../../src/first_display/adapters/googletag';
import type {
  FirstDisplayGoogletagBatch,
  FirstDisplayGoogletagBatchCallbacks,
} from '../../src/first_display/adapters/googletag';
import { snapshotFirstDisplayBatchV1 } from '../../src/first_display/leaf/projection';

const RESERVATION_ID = `r1_${'a'.repeat(22)}`;

function startAdapter(
  adapter: FirstDisplayGoogletagBatch,
  callbacks: Readonly<{
    onBound: FirstDisplayGoogletagBatchCallbacks[0];
    onFailure: FirstDisplayGoogletagBatchCallbacks[1];
    onFirstAction: FirstDisplayGoogletagBatchCallbacks[2];
    onRenderEnded: FirstDisplayGoogletagBatchCallbacks[3];
    onRetire?: FirstDisplayGoogletagBatchCallbacks[4];
  }>
): boolean {
  return adapter.start(
    Object.freeze([
      callbacks.onBound,
      callbacks.onFailure,
      callbacks.onFirstAction,
      callbacks.onRenderEnded,
      callbacks.onRetire,
    ])
  );
}

function fixture() {
  return Object.freeze({
    version: 1,
    projectionDigest: 'b'.repeat(64),
    projection: Object.freeze({
      version: 1,
      auction: Object.freeze({
        version: 1,
        auctionId: 'initial',
        results: Object.freeze([
          Object.freeze({ slot: 'slot-1', outcome: 'winner', candidateId: 'candidate001' }),
        ]),
      }),
      slots: Object.freeze([
        Object.freeze({
          slot: 'slot-1',
          gamUnitPath: '/123/example',
          divId: 'slot-1',
          formats: Object.freeze([Object.freeze([300, 250])]),
          targeting: Object.freeze({ placement: 'article' }),
        }),
      ]),
      bids: Object.freeze([
        Object.freeze({
          candidateId: 'candidate001',
          slot: 'slot-1',
          provider: 'example',
          upstreamBidId: 'upstream-1',
          cpm: 1.25,
          currency: 'USD',
          targeting: Object.freeze({ hb_pb: '1.25' }),
          rendererReservationId: RESERVATION_ID,
          renderSource: Object.freeze({
            type: 'adm',
            version: 1,
            adm: '<div>example</div>',
            width: 300,
            height: 250,
          }),
        }),
      ]),
    }),
  });
}

function twoSlotFixture() {
  const secondReservationId = `r1_${'b'.repeat(22)}`;
  return Object.freeze({
    version: 1,
    projectionDigest: 'b'.repeat(64),
    projection: Object.freeze({
      version: 1,
      auction: Object.freeze({
        version: 1,
        auctionId: 'initial',
        results: Object.freeze([
          Object.freeze({ slot: 'slot-1', outcome: 'winner', candidateId: 'candidate001' }),
          Object.freeze({ slot: 'slot-2', outcome: 'winner', candidateId: 'candidate002' }),
        ]),
      }),
      slots: Object.freeze([
        Object.freeze({
          slot: 'slot-1',
          gamUnitPath: '/123/example-one',
          divId: 'slot-1',
          formats: Object.freeze([Object.freeze([300, 250])]),
          targeting: Object.freeze({ placement: 'article' }),
        }),
        Object.freeze({
          slot: 'slot-2',
          gamUnitPath: '/123/example-two',
          divId: 'slot-2',
          formats: Object.freeze([Object.freeze([728, 90])]),
          targeting: Object.freeze({ placement: 'sidebar' }),
        }),
      ]),
      bids: Object.freeze([
        Object.freeze({
          candidateId: 'candidate001',
          slot: 'slot-1',
          provider: 'example',
          upstreamBidId: 'upstream-1',
          cpm: 1.25,
          currency: 'USD',
          targeting: Object.freeze({ hb_pb: '1.25' }),
          rendererReservationId: RESERVATION_ID,
          renderSource: Object.freeze({
            type: 'adm',
            version: 1,
            adm: '<div>example one</div>',
            width: 300,
            height: 250,
          }),
        }),
        Object.freeze({
          candidateId: 'candidate002',
          slot: 'slot-2',
          provider: 'example',
          upstreamBidId: 'upstream-2',
          cpm: 2.5,
          currency: 'USD',
          targeting: Object.freeze({ hb_pb: '2.50' }),
          rendererReservationId: secondReservationId,
          renderSource: Object.freeze({
            type: 'adm',
            version: 1,
            adm: '<div>example two</div>',
            width: 728,
            height: 90,
          }),
        }),
      ]),
    }),
  });
}

function protocol() {
  return Object.freeze({
    deadlines: Object.freeze({
      externalReadyMs: 10_000 as const,
      requestStartMs: 3_000 as const,
      completionMs: 10_000 as const,
    }),
    requestPlan: (candidate: unknown) => {
      const value = candidate as { initialLoadDisabled: boolean; ownership: string };
      if (value.ownership === 'publisher') {
        return Object.freeze({
          operations: Object.freeze(['refresh'] as const),
          requestOperation: 0,
        });
      }
      return value.initialLoadDisabled
        ? Object.freeze({
            operations: Object.freeze(['display', 'refresh'] as const),
            requestOperation: 1 as const,
          })
        : Object.freeze({ operations: Object.freeze(['display'] as const), requestOperation: 0 });
    },
    classifyRenderEnded: (candidate: unknown) => {
      const value = candidate as { isEmpty?: unknown };
      return value.isEmpty === true
        ? ('gam_empty' as const)
        : value.isEmpty === false
          ? ('nonempty_gam' as const)
          : undefined;
    },
  });
}

describe('first-display GPT adapter', () => {
  it('owns enabled GAM attribution before publisher parser work without a winning bid', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
      url: 'https://publisher.example/',
    });
    const commands: Array<() => void> = [];
    const order: string[] = [];
    const setConfig = vi.fn(() => order.push('trusted-server'));
    Object.defineProperty(dom.window, 'tsjs', {
      configurable: true,
      value: { adInit: true },
    });
    Object.defineProperty(dom.window, 'googletag', {
      configurable: true,
      value: { cmd: commands, setConfig },
      writable: true,
    });
    expect(enqueueFirstDisplayGamAttribution(dom.window as unknown as Window)).toBe(true);
    commands.push(() => order.push('publisher'));
    commands.splice(0).forEach((command) => command());

    expect(order).toEqual(['trusted-server', 'publisher']);
    expect(setConfig).toHaveBeenCalledExactlyOnceWith({ targeting: { ts: 'true' } });
  });

  it('creates an absent GPT queue and isolates a missing or throwing targeting API', () => {
    const absent: { googletag?: unknown } = {};
    expect(enqueueFirstDisplayGamAttribution(absent as unknown as Window)).toBe(true);
    const created = absent.googletag as { cmd: Array<() => void> };
    expect(created.cmd).toHaveLength(1);
    expect(() => created.cmd[0]?.()).not.toThrow();

    const publisher = vi.fn();
    const commands: Array<() => void> = [];
    const throwing = {
      googletag: {
        cmd: commands,
        setConfig: () => {
          throw new Error('targeting unavailable');
        },
      },
    };
    expect(enqueueFirstDisplayGamAttribution(throwing as unknown as Window)).toBe(true);
    commands.push(publisher);
    expect(() => commands.splice(0).forEach((command) => command())).not.toThrow();
    expect(publisher).toHaveBeenCalledOnce();
  });

  it('observes publisher GPT calls, events, and targeting until ingress closes', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
      url: 'https://publisher.example/',
    });
    const listeners = new Map<string, (event: unknown) => void>();
    const mutations = vi.fn(() => true);
    const retired = vi.fn();
    const slot = {
      clearTargeting: vi.fn(() => undefined),
      getSlotElementId: () => 'slot-1',
      getTargeting: () => [],
      setTargeting: vi.fn((_key: string, _value: string) => slot),
    };
    const service = {
      addEventListener: (name: string, listener: (event: unknown) => void) =>
        listeners.set(name, listener),
      getSlots: () => [slot],
      refresh: vi.fn(),
      removeEventListener: () => undefined,
    };
    const binding = {
      pubadsReady: true,
      cmd: { push: (command: () => void) => command() },
      defineSlot: vi.fn(() => slot),
      destroySlots: vi.fn(() => true),
      display: vi.fn(),
      getConfig: () => ({ disableInitialLoad: false }),
      pubads: () => service,
    };
    Object.defineProperty(dom.window, 'googletag', { configurable: true, value: binding });
    const batch = snapshotFirstDisplayBatchV1(fixture())!;
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      document: dom.window.document,
      projection: batch.projection,
      protocol: protocol(),
      onNativeMutation: mutations,
      setTimer: (callback) => callback,
      clearTimer: () => undefined,
    });
    startAdapter(adapter, {
      onBound: () => undefined,
      onFailure: () => undefined,
      onFirstAction: () => true,
      onRenderEnded: () => undefined,
      onRetire: retired,
    });
    listeners.get('slotRequested')?.({ slot });
    listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });
    mutations.mockClear();

    slot.setTargeting('publisher', 'value');
    service.refresh([slot]);
    binding.display('publisher-slot');
    listeners.get('slotRequested')?.({ slot, responseIdentifier: 'response-two' });
    listeners.get('slotRenderEnded')?.({
      slot,
      responseIdentifier: 'response-two',
      isEmpty: false,
    });
    expect(mutations).toHaveBeenCalledTimes(5);
    expect(retired).toHaveBeenCalledOnce();

    expect(adapter.closeIngress(['slot-1'])).toBe(true);
    const diagnostics = adapter.captureDiagnosticsHandoff();
    const [cycle] = diagnostics?.[0] ?? [];
    expect(cycle?.[2]).toBe(3);
    expect(cycle?.[5]).toEqual([
      expect.arrayContaining([1, 'completed']),
      [2, 'response-two', ['slotRequested', 'slotRenderEnded'], 'completed'],
    ]);
    mutations.mockClear();
    slot.setTargeting('publisher', 'later');
    service.refresh([slot]);
    binding.display('publisher-slot');
    listeners.get('slotRequested')?.({ slot });
    expect(mutations).not.toHaveBeenCalled();
  });

  it('defines, targets, and starts one TS slot before attributing its render event', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
      url: 'https://publisher.example/',
    });
    const events: string[] = [];
    const listeners = new Map<string, (event: unknown) => void>();
    const slot = {
      addService: vi.fn(() => slot),
      getSlotElementId: vi.fn(() => 'slot-1'),
      setTargeting: vi.fn((key: string) => {
        events.push(`target:${key}`);
        return slot;
      }),
    };
    const service = {
      addEventListener: vi.fn((name: string, listener: (event: unknown) => void) => {
        listeners.set(name, listener);
      }),
      enableSingleRequest: vi.fn(() => {
        events.push('sra');
        return true;
      }),
      getSlots: vi.fn(() => []),
      refresh: vi.fn(() => events.push('refresh')),
      removeEventListener: vi.fn(),
    };
    Object.defineProperty(dom.window, 'googletag', {
      configurable: true,
      value: {
        pubadsReady: false,
        cmd: { push: (command: () => void) => command() },
        defineSlot: vi.fn(() => slot),
        display: vi.fn(() => events.push('display')),
        enableServices: vi.fn(() => events.push('services')),
        getConfig: vi.fn(() => ({ disableInitialLoad: false })),
        pubads: vi.fn(() => service),
      },
      writable: true,
    });
    const batch = snapshotFirstDisplayBatchV1(fixture());
    expect(batch).toBeDefined();
    const timers: Array<() => void> = [];
    const renders: unknown[] = [];
    const failures: unknown[] = [];
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      document: dom.window.document,
      projection: batch!.projection,
      protocol: protocol(),
      setTimer: (callback) => {
        timers.push(callback);
        return callback;
      },
      clearTimer: (handle) => {
        const index = timers.indexOf(handle as () => void);
        if (index >= 0) timers.splice(index, 1);
      },
    });

    expect(
      startAdapter(adapter, {
        onBound: (cycle) => {
          expect(cycle[1]).toBe(dom.window.document.getElementById('slot-1'));
          events.push(`bound:${cycle[6]}:${cycle[3]}`);
        },
        onFailure: (slotId, reason) => failures.push([slotId, reason]),
        onFirstAction: () => {
          events.push('first-action');
          return true;
        },
        onRenderEnded: (cycle, result) => renders.push([cycle[6], result]),
      })
    ).toBe(true);
    expect(events).toEqual([
      'bound:slot-1:trusted_server',
      'target:hb_adid',
      'target:hb_pb',
      'target:placement',
      'first-action',
      'sra',
      'services',
      'display',
    ]);
    expect(slot.setTargeting.mock.calls).toEqual([
      ['hb_adid', `r1_${'a'.repeat(22)}`],
      ['hb_pb', '1.25'],
      ['placement', 'article'],
    ]);

    listeners.get('slotRequested')?.({ slot });
    listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });
    expect(renders).toEqual([['slot-1', 'nonempty_gam']]);
    expect(failures).toEqual([]);
    expect(timers).toHaveLength(0);
    adapter.dispose();
    expect(service.removeEventListener.mock.calls.map(([name]) => name)).toEqual([
      'slotRequested',
      'slotRenderEnded',
    ]);
  });

  it.each(['enableSingleRequest', 'enableServices'] as const)(
    'fails the entire batch without a request when %s throws',
    (failure) => {
      const dom = new JSDOM('<!doctype html><div id="slot-1"></div><div id="slot-2"></div>', {
        url: 'https://publisher.example/',
      });
      const slots = new Map<string, object>();
      const display = vi.fn();
      const refresh = vi.fn();
      const service = {
        addEventListener: vi.fn(),
        enableSingleRequest: vi.fn(() => {
          if (failure === 'enableSingleRequest') throw new Error('fictional SRA failure');
          return true;
        }),
        getSlots: () => [],
        refresh,
        removeEventListener: vi.fn(),
      };
      const binding = {
        pubadsReady: false,
        cmd: { push: (command: () => void) => command() },
        defineSlot: (_path: string, _sizes: unknown, elementId: string) => {
          const targeting = new Map<string, string[]>();
          const slot = {
            addService: () => slot,
            clearTargeting: (key: string) => targeting.delete(key),
            getTargeting: (key: string) => targeting.get(key) ?? [],
            setTargeting: (key: string, value: string) => {
              targeting.set(key, [value]);
              return slot;
            },
          };
          slots.set(elementId, slot);
          return slot;
        },
        display,
        enableServices: () => {
          if (failure === 'enableServices') throw new Error('fictional service failure');
        },
        getConfig: () => ({ disableInitialLoad: false }),
        pubads: () => service,
      };
      Object.defineProperty(dom.window, 'googletag', { configurable: true, value: binding });
      const failures: unknown[] = [];
      const firstAction = vi.fn(() => true);
      const batch = snapshotFirstDisplayBatchV1(twoSlotFixture())!;
      const adapter = createFirstDisplayGoogletagBatch({
        browser: dom.window as unknown as Window,
        clearTimer: () => undefined,
        document: dom.window.document,
        projection: batch.projection,
        protocol: protocol(),
        setTimer: (callback) => callback,
      });

      startAdapter(adapter, {
        onBound: () => undefined,
        onFailure: (slotId, reason) => failures.push([slotId, reason]),
        onFirstAction: firstAction,
        onRenderEnded: () => undefined,
      });

      expect(firstAction).toHaveBeenCalledOnce();
      expect(failures).toEqual([
        ['slot-1', 'gpt_request_failed'],
        ['slot-2', 'gpt_request_failed'],
      ]);
      expect(display).not.toHaveBeenCalled();
      expect(refresh).not.toHaveBeenCalled();
    }
  );

  it('arms every synchronous SRA display cycle before the first display requests all slots', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div><div id="slot-2"></div>', {
      url: 'https://publisher.example/',
    });
    const listeners = new Map<string, (event: unknown) => void>();
    const slots: object[] = [];
    const service = {
      addEventListener: (name: string, listener: (event: unknown) => void) =>
        listeners.set(name, listener),
      getSlots: () => [],
      refresh: vi.fn(),
      removeEventListener: vi.fn(),
    };
    const display = vi.fn(() => {
      for (const slot of slots) listeners.get('slotRequested')?.({ slot });
      for (const slot of slots) listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });
    });
    const binding = {
      pubadsReady: true,
      cmd: { push: (command: () => void) => command() },
      defineSlot: (_path: string, _sizes: unknown, _elementId: string) => {
        const targeting = new Map<string, string[]>();
        const slot = {
          addService: () => slot,
          clearTargeting: (key: string) => targeting.delete(key),
          getTargeting: (key: string) => targeting.get(key) ?? [],
          setTargeting: (key: string, value: string) => {
            targeting.set(key, [value]);
            return slot;
          },
        };
        slots.push(slot);
        return slot;
      },
      display,
      getConfig: () => ({ disableInitialLoad: false }),
      pubads: () => service,
    };
    Object.defineProperty(dom.window, 'googletag', { configurable: true, value: binding });
    const rendered: string[] = [];
    const failures: unknown[] = [];
    const firstAction = vi.fn(() => true);
    const batch = snapshotFirstDisplayBatchV1(twoSlotFixture())!;
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      clearTimer: () => undefined,
      document: dom.window.document,
      projection: batch.projection,
      protocol: protocol(),
      setTimer: (callback) => callback,
    });

    startAdapter(adapter, {
      onBound: () => undefined,
      onFailure: (slotId, reason) => failures.push([slotId, reason]),
      onFirstAction: firstAction,
      onRenderEnded: (cycle) => rendered.push(cycle[6]),
    });

    expect(display).toHaveBeenCalledOnce();
    expect(firstAction).toHaveBeenCalledOnce();
    expect(rendered).toEqual(['slot-1', 'slot-2']);
    expect(failures).toEqual([]);
  });

  it('invalidates the delayed-owner binding when a publisher replaces the physical slot', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
      url: 'https://publisher.example/',
    });
    const listeners = new Map<string, (event: unknown) => void>();
    const slot = {
      addService: () => slot,
      getSlotElementId: () => 'slot-1',
      setTargeting: () => slot,
    };
    const replacement = {
      addService: () => replacement,
      getSlotElementId: () => 'slot-1',
      setTargeting: () => replacement,
    };
    const service = {
      addEventListener: (name: string, listener: (event: unknown) => void) =>
        listeners.set(name, listener),
      getSlots: () => [],
      removeEventListener: () => undefined,
    };
    const defineSlot = vi.fn().mockReturnValueOnce(slot).mockReturnValueOnce(replacement);
    let destroyMode: 'throw' | 'false' | 'true' = 'throw';
    const destroySlots = vi.fn((slots?: readonly object[]) => {
      if (destroyMode === 'throw') throw new Error('fictional destroy failure');
      if (destroyMode === 'false') return false;
      if (Array.isArray(slots)) (slots as object[]).length = 0;
      return true;
    });
    const binding = {
      pubadsReady: true,
      cmd: { push: (command: () => void) => command() },
      defineSlot,
      destroySlots,
      display: (_elementId: string) => undefined,
      getConfig: () => ({ disableInitialLoad: false }),
      pubads: () => service,
    };
    Object.defineProperty(dom.window, 'googletag', {
      configurable: true,
      value: binding,
    });
    const batch = snapshotFirstDisplayBatchV1(fixture())!;
    const bound: { value?: Parameters<FirstDisplayGoogletagBatchCallbacks[0]>[0] } = {};
    const retired = vi.fn();
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      clearTimer: () => undefined,
      document: dom.window.document,
      projection: batch.projection,
      protocol: protocol(),
      setTimer: (callback) => callback,
    });
    startAdapter(adapter, {
      onBound: (cycle) => {
        bound.value = cycle;
      },
      onFailure: () => undefined,
      onFirstAction: () => true,
      onRenderEnded: () => undefined,
      onRetire: retired,
    });

    expect(bound.value?.[2]()).toBe(true);
    listeners.get('slotRequested')?.({ slot });
    listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });
    expect(bound.value?.[2]()).toBe(true);

    expect(() => binding.destroySlots([slot])).toThrow('fictional destroy failure');
    expect(bound.value?.[2]()).toBe(true);
    expect(retired).not.toHaveBeenCalled();
    destroyMode = 'false';
    expect(binding.destroySlots([slot])).toBe(false);
    expect(bound.value?.[2]()).toBe(true);
    expect(retired).not.toHaveBeenCalled();
    destroyMode = 'true';
    const targets = [slot];
    expect(binding.destroySlots(targets)).toBe(true);
    expect(targets).toEqual([]);
    expect(binding.defineSlot('/publisher/replacement', [[300, 250]], 'slot-1')).toBe(replacement);
    binding.display('slot-1');
    expect(bound.value?.[2]()).toBe(false);
    expect(retired).toHaveBeenCalledOnce();
  });

  it('cancels a pending command and compare-restores an adapter-created GPT queue', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
      url: 'https://publisher.example/',
    });
    const batch = snapshotFirstDisplayBatchV1(fixture());
    const timers: Array<() => void> = [];
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      document: dom.window.document,
      projection: batch!.projection,
      protocol: protocol(),
      setTimer: (callback) => {
        timers.push(callback);
        return callback;
      },
      clearTimer: () => undefined,
    });
    expect(
      startAdapter(adapter, {
        onBound: () => undefined,
        onFailure: () => undefined,
        onFirstAction: () => true,
        onRenderEnded: () => undefined,
      })
    ).toBe(true);
    expect(
      (dom.window as unknown as { googletag?: { cmd?: unknown[] } }).googletag?.cmd
    ).toHaveLength(1);

    adapter.dispose();
    expect(Object.prototype.hasOwnProperty.call(dom.window, 'googletag')).toBe(false);
  });

  it('uses publisher refresh and compare-restores only unchanged targeting', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
      url: 'https://publisher.example/',
    });
    const targeting = new Map<string, string[]>([['hb_pb', ['publisher-original']]]);
    const slot = {
      clearTargeting: vi.fn((key: string) => targeting.delete(key)),
      getSlotElementId: vi.fn(() => 'slot-1'),
      getTargeting: vi.fn((key: string) => targeting.get(key) ?? []),
      setTargeting: vi.fn((key: string, value: string | string[]) => {
        targeting.set(key, Array.isArray(value) ? [...value] : [value]);
        return slot;
      }),
    };
    const listeners = new Map<string, (event: unknown) => void>();
    const refresh = vi.fn();
    const service = {
      addEventListener: (name: string, listener: (event: unknown) => void) =>
        listeners.set(name, listener),
      getSlots: () => [slot],
      refresh,
      removeEventListener: vi.fn(),
    };
    const defineSlot = vi.fn();
    const display = vi.fn();
    Object.defineProperty(dom.window, 'googletag', {
      configurable: true,
      value: {
        pubadsReady: true,
        cmd: { push: (command: () => void) => command() },
        defineSlot,
        display,
        getConfig: () => ({ disableInitialLoad: false }),
        pubads: () => service,
      },
    });
    const batch = snapshotFirstDisplayBatchV1(fixture())!;
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      document: dom.window.document,
      projection: batch.projection,
      protocol: protocol(),
      setTimer: (callback) => callback,
      clearTimer: () => undefined,
    });
    const firstAction = vi.fn(() => true);

    startAdapter(adapter, {
      onBound: (cycle) => expect(cycle[3]).toBe('publisher'),
      onFailure: () => undefined,
      onFirstAction: firstAction,
      onRenderEnded: () => undefined,
    });
    expect(defineSlot).not.toHaveBeenCalled();
    expect(display).not.toHaveBeenCalled();
    expect(refresh).toHaveBeenCalledWith([slot], { changeCorrelator: false });
    expect(firstAction).toHaveBeenCalledOnce();

    listeners.get('slotRequested')?.({ slot });
    listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });
    slot.setTargeting('placement', 'article');
    expect(adapter.closeIngress(['slot-1'])).toBe(true);
    expect(adapter.captureHandoff()?.[0]?.[3]).toEqual([
      { installed: RESERVATION_ID, key: 'hb_adid', prior: [] },
      { installed: '1.25', key: 'hb_pb', prior: ['publisher-original'] },
    ]);
    adapter.dispose();
    expect(targeting.get('placement')).toEqual(['article']);
    expect(targeting.get('hb_pb')).toEqual(['publisher-original']);
    expect(targeting.has('hb_adid')).toBe(false);
  });

  it.each(['reentrant_refresh', 'slot_object_display'] as const)(
    'does not attribute a competing publisher %s to the pending TS request',
    (competition) => {
      const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
        url: 'https://publisher.example/',
      });
      const listeners = new Map<string, (event: unknown) => void>();
      const slot = {
        getSlotElementId: () => 'slot-1',
        getTargeting: () => [],
        setTargeting: () => slot,
        clearTargeting: () => slot,
      };
      let reentered = false;
      const refresh = vi.fn((_slots?: readonly object[]) => {
        if (competition === 'reentrant_refresh' && !reentered) {
          reentered = true;
          service.refresh([slot]);
        }
      });
      const service = {
        addEventListener: (name: string, listener: (event: unknown) => void) =>
          listeners.set(name, listener),
        getSlots: () => [slot],
        refresh,
        removeEventListener: () => undefined,
      };
      const display = vi.fn((_slot?: unknown) => undefined);
      const binding = {
        pubadsReady: true,
        cmd: { push: (command: () => void) => command() },
        defineSlot: () => undefined,
        display,
        getConfig: () => ({ disableInitialLoad: false }),
        pubads: () => service,
      };
      Object.defineProperty(dom.window, 'googletag', {
        configurable: true,
        value: binding,
      });
      const batch = snapshotFirstDisplayBatchV1(fixture())!;
      const failure = vi.fn();
      const adapter = createFirstDisplayGoogletagBatch({
        browser: dom.window as unknown as Window,
        clearTimer: () => undefined,
        diagnosticsActive: true,
        document: dom.window.document,
        projection: batch.projection,
        protocol: protocol(),
        setTimer: (callback) => callback,
      });
      startAdapter(adapter, {
        onBound: () => undefined,
        onFailure: failure,
        onFirstAction: () => true,
        onRenderEnded: () => undefined,
      });

      if (competition === 'slot_object_display') binding.display(slot as never);
      listeners.get('slotRequested')?.({ slot });

      expect(refresh).toHaveBeenCalledTimes(competition === 'reentrant_refresh' ? 2 : 1);
      expect(display).toHaveBeenCalledTimes(competition === 'slot_object_display' ? 1 : 0);
      expect(failure).toHaveBeenCalledWith('slot-1', 'cycle_unattributable');
      expect(adapter.closeIngress([])).toBe(true);
      expect(adapter.captureDiagnosticsHandoff()?.[1][0]).toMatchObject({
        event: 'slotRequested',
        requestedSlotSizes: null,
      });
      adapter.dispose();
    }
  );

  it.each(['exact', 'hydration'] as const)(
    'hands off a %s late publisher definition without creating a second GPT slot',
    (mode) => {
      const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
        url: 'https://publisher.example/',
      });
      const warning = vi.spyOn(dom.window.console, 'warn').mockImplementation(() => undefined);
      const listeners = new Map<string, (event: unknown) => void>();
      const targeting = new Map<string, string[]>();
      const slot = {
        addService: () => slot,
        clearTargeting: (key: string) => targeting.delete(key),
        getAdUnitPath: () => '/123/example',
        getSlotElementId: () => 'slot-1',
        getTargeting: (key: string) => targeting.get(key) ?? [],
        setTargeting: (key: string, value: string | readonly string[]) => {
          targeting.set(key, typeof value === 'string' ? [value] : [...value]);
          return slot;
        },
      };
      let defined = false;
      let disabled = false;
      const refresh = vi.fn((_slots?: readonly object[], _options?: unknown) => undefined);
      const service = {
        addEventListener: (name: string, listener: (event: unknown) => void) =>
          listeners.set(name, listener),
        getSlots: () => (defined ? [slot] : []),
        refresh,
        removeEventListener: () => undefined,
      };
      const defineSlot = vi.fn((_path?: unknown, _sizes?: unknown, _elementId?: unknown) => {
        defined = true;
        return slot;
      });
      const display = vi.fn((_target?: unknown) => undefined);
      const binding = {
        pubadsReady: true,
        cmd: { push: (command: () => void) => command() },
        defineSlot,
        destroySlots: vi.fn(() => true),
        display,
        getConfig: () => ({ disableInitialLoad: disabled }),
        pubads: () => service,
      };
      Object.defineProperty(dom.window, 'googletag', {
        configurable: true,
        value: binding,
      });
      const batch = snapshotFirstDisplayBatchV1(fixture())!;
      const adapter = createFirstDisplayGoogletagBatch({
        browser: dom.window as unknown as Window,
        clearTimer: () => undefined,
        document: dom.window.document,
        projection: batch.projection,
        protocol: protocol(),
        setTimer: (callback) => callback,
      });
      startAdapter(adapter, {
        onBound: () => undefined,
        onFailure: () => undefined,
        onFirstAction: () => true,
        onRenderEnded: () => undefined,
      });
      listeners.get('slotRequested')?.({ slot });
      listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });
      disabled = true;

      let elementId = 'slot-1';
      let path: string = '/publisher/mismatch';
      let sizes: readonly (readonly [number, number])[] = [[728, 90]];
      if (mode === 'hydration') {
        dom.window.document.getElementById('slot-1')?.remove();
        const replacement = dom.window.document.createElement('div');
        replacement.id = 'slot-1-hydrated';
        dom.window.document.body.append(replacement);
        elementId = replacement.id;
        path = '/123/example';
        sizes = [[300, 250]];
      }
      expect(binding.defineSlot(path, sizes, elementId)).toBe(slot);
      expect(defineSlot).toHaveBeenCalledTimes(1);
      if (mode === 'exact') {
        expect(warning).toHaveBeenCalledExactlyOnceWith('GPT publisher handoff metadata mismatch', {
          formatsMismatch: true,
          pathMismatch: true,
        });
        slot.setTargeting('hb_pb', 'publisher');
      } else {
        expect(warning).not.toHaveBeenCalled();
      }

      const displayCalls = display.mock.calls.length;
      const refreshCalls = refresh.mock.calls.length;
      binding.display(elementId);
      service.refresh([slot], { changeCorrelator: false } as never);
      expect(display).toHaveBeenCalledTimes(displayCalls);
      expect(refresh).toHaveBeenCalledTimes(refreshCalls);
      binding.display(elementId);
      service.refresh([slot], { changeCorrelator: false } as never);
      expect(display).toHaveBeenCalledTimes(displayCalls + 1);
      expect(refresh).toHaveBeenCalledTimes(refreshCalls + 1);

      expect(adapter.closeIngress(['slot-1'])).toBe(true);
      expect(adapter.captureHandoff()?.[0]?.slice(0, 3)).toEqual([
        'slot-1',
        elementId,
        'publisher',
      ]);
      expect(adapter.captureHandoff()?.[0]?.[3]).toEqual(
        mode === 'exact'
          ? [
              { installed: RESERVATION_ID, key: 'hb_adid', prior: [] },
              { installed: 'article', key: 'placement', prior: [] },
            ]
          : [
              { installed: RESERVATION_ID, key: 'hb_adid', prior: [] },
              { installed: '1.25', key: 'hb_pb', prior: [] },
              { installed: 'article', key: 'placement', prior: [] },
            ]
      );
      adapter.dispose();
      expect(binding.destroySlots).not.toHaveBeenCalled();
    }
  );

  it('rejects targeting handoff after a publisher replaces an observed method', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
      url: 'https://publisher.example/',
    });
    const targeting = new Map<string, string[]>([['hb_pb', ['publisher-original']]]);
    const slot = {
      clearTargeting: vi.fn((key: string) => targeting.delete(key)),
      getSlotElementId: () => 'slot-1',
      getTargeting: (key: string) => targeting.get(key) ?? [],
      setTargeting: vi.fn((key: string, value: string | string[]) => {
        targeting.set(key, Array.isArray(value) ? [...value] : [value]);
        return slot;
      }),
    };
    const listeners = new Map<string, (event: unknown) => void>();
    const service = {
      addEventListener: (name: string, listener: (event: unknown) => void) =>
        listeners.set(name, listener),
      getSlots: () => [slot],
      refresh: () => undefined,
      removeEventListener: () => undefined,
    };
    Object.defineProperty(dom.window, 'googletag', {
      configurable: true,
      value: {
        pubadsReady: true,
        cmd: { push: (command: () => void) => command() },
        defineSlot: () => undefined,
        display: () => undefined,
        getConfig: () => ({ disableInitialLoad: false }),
        pubads: () => service,
      },
    });
    const batch = snapshotFirstDisplayBatchV1(fixture())!;
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      document: dom.window.document,
      projection: batch.projection,
      protocol: protocol(),
      setTimer: (callback) => callback,
      clearTimer: () => undefined,
    });
    startAdapter(adapter, {
      onBound: () => undefined,
      onFailure: () => undefined,
      onFirstAction: () => true,
      onRenderEnded: () => undefined,
    });
    listeners.get('slotRequested')?.({ slot });
    listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });

    const replacement = vi.fn((key: string, value: string | string[]) => {
      targeting.set(key, Array.isArray(value) ? [...value] : [value]);
      return slot;
    });
    Object.defineProperty(slot, 'setTargeting', {
      configurable: true,
      value: replacement,
      writable: true,
    });
    slot.setTargeting('hb_pb', '1.25');

    expect(adapter.closeIngress(['slot-1'])).toBe(true);
    expect(adapter.captureHandoff()?.[0]?.[3]).toEqual([]);
    expect(adapter.detachCommittedSlots(['slot-1'])).toBe(true);
    adapter.dispose();
    expect(replacement).toHaveBeenCalledWith('hb_pb', '1.25');
    expect(targeting.get('hb_pb')).toEqual(['1.25']);
  });

  it.each(['close', 'dispose'] as const)(
    'does not restore stale publisher targeting through a replacement during %s cleanup',
    (cleanup) => {
      const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
        url: 'https://publisher.example/',
      });
      const targeting = new Map<string, string[]>([['hb_pb', ['publisher-original']]]);
      const slot = {
        clearTargeting: vi.fn((key: string) => targeting.delete(key)),
        getSlotElementId: () => 'slot-1',
        getTargeting: (key: string) => targeting.get(key) ?? [],
        setTargeting: vi.fn((key: string, value: string | string[]) => {
          targeting.set(key, Array.isArray(value) ? [...value] : [value]);
          return slot;
        }),
      };
      const listeners = new Map<string, (event: unknown) => void>();
      const service = {
        addEventListener: (name: string, listener: (event: unknown) => void) =>
          listeners.set(name, listener),
        getSlots: () => [slot],
        refresh: () => undefined,
        removeEventListener: () => undefined,
      };
      Object.defineProperty(dom.window, 'googletag', {
        configurable: true,
        value: {
          pubadsReady: true,
          cmd: { push: (command: () => void) => command() },
          defineSlot: () => undefined,
          display: () => undefined,
          getConfig: () => ({ disableInitialLoad: false }),
          pubads: () => service,
        },
      });
      const batch = snapshotFirstDisplayBatchV1(fixture())!;
      const adapter = createFirstDisplayGoogletagBatch({
        browser: dom.window as unknown as Window,
        document: dom.window.document,
        projection: batch.projection,
        protocol: protocol(),
        setTimer: (callback) => callback,
        clearTimer: () => undefined,
      });
      startAdapter(adapter, {
        onBound: () => undefined,
        onFailure: () => undefined,
        onFirstAction: () => true,
        onRenderEnded: () => undefined,
      });
      listeners.get('slotRequested')?.({ slot });
      listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });

      const replacement = vi.fn((key: string, value: string | string[]) => {
        targeting.set(key, Array.isArray(value) ? [...value] : [value]);
        return slot;
      });
      Object.defineProperty(slot, 'setTargeting', {
        configurable: true,
        value: replacement,
        writable: true,
      });
      slot.setTargeting('hb_pb', '1.25');

      if (cleanup === 'close') {
        expect(adapter.closeIngress([])).toBe(true);
        adapter.dispose();
      } else {
        adapter.dispose();
      }
      expect(replacement).toHaveBeenCalledOnce();
      expect(targeting.get('hb_pb')).toEqual(['1.25']);
    }
  );

  it.each(['close', 'dispose'] as const)(
    'honors a same-value publisher write from getTargeting during %s cleanup',
    (cleanup) => {
      const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
        url: 'https://publisher.example/',
      });
      const targeting = new Map<string, string[]>([['hb_pb', ['publisher-original']]]);
      let cleanupPhase = false;
      let reentrantCleanupCalls = 0;
      const slot = {
        clearTargeting: vi.fn((key: string) => targeting.delete(key)),
        getSlotElementId: () => 'slot-1',
        getTargeting: vi.fn((key: string) => {
          if (cleanupPhase && key === 'hb_pb' && reentrantCleanupCalls === 0) {
            reentrantCleanupCalls += 1;
            slot.setTargeting('hb_pb', '1.25');
          }
          return targeting.get(key) ?? [];
        }),
        setTargeting: vi.fn((key: string, value: string | string[]) => {
          targeting.set(key, Array.isArray(value) ? [...value] : [value]);
          return slot;
        }),
      };
      const listeners = new Map<string, (event: unknown) => void>();
      const service = {
        addEventListener: (name: string, listener: (event: unknown) => void) =>
          listeners.set(name, listener),
        getSlots: () => [slot],
        refresh: () => undefined,
        removeEventListener: () => undefined,
      };
      Object.defineProperty(dom.window, 'googletag', {
        configurable: true,
        value: {
          pubadsReady: true,
          cmd: { push: (command: () => void) => command() },
          defineSlot: () => undefined,
          display: () => undefined,
          getConfig: () => ({ disableInitialLoad: false }),
          pubads: () => service,
        },
      });
      const batch = snapshotFirstDisplayBatchV1(fixture())!;
      const adapter = createFirstDisplayGoogletagBatch({
        browser: dom.window as unknown as Window,
        document: dom.window.document,
        projection: batch.projection,
        protocol: protocol(),
        setTimer: (callback) => callback,
        clearTimer: () => undefined,
      });
      startAdapter(adapter, {
        onBound: () => undefined,
        onFailure: () => undefined,
        onFirstAction: () => true,
        onRenderEnded: () => undefined,
      });
      listeners.get('slotRequested')?.({ slot });
      listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });

      cleanupPhase = true;
      if (cleanup === 'close') {
        expect(adapter.closeIngress([])).toBe(true);
        adapter.dispose();
      } else {
        adapter.dispose();
      }
      expect(reentrantCleanupCalls).toBe(1);
      expect(targeting.get('hb_pb')).toEqual(['1.25']);
    }
  );

  it('seals retained targeting while same-value publisher writes are still observed', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
      url: 'https://publisher.example/',
    });
    const targeting = new Map<string, string[]>([['hb_pb', ['publisher-original']]]);
    let sealing = false;
    let reentrantSealingCalls = 0;
    const slot = {
      clearTargeting: vi.fn((key: string) => targeting.delete(key)),
      getSlotElementId: () => 'slot-1',
      getTargeting: vi.fn((key: string) => {
        if (sealing && key === 'hb_pb' && reentrantSealingCalls === 0) {
          reentrantSealingCalls += 1;
          slot.setTargeting('hb_pb', '1.25');
        }
        return targeting.get(key) ?? [];
      }),
      setTargeting: vi.fn((key: string, value: string | string[]) => {
        targeting.set(key, Array.isArray(value) ? [...value] : [value]);
        return slot;
      }),
    };
    const listeners = new Map<string, (event: unknown) => void>();
    const service = {
      addEventListener: (name: string, listener: (event: unknown) => void) =>
        listeners.set(name, listener),
      getSlots: () => [slot],
      refresh: () => undefined,
      removeEventListener: () => undefined,
    };
    Object.defineProperty(dom.window, 'googletag', {
      configurable: true,
      value: {
        pubadsReady: true,
        cmd: { push: (command: () => void) => command() },
        defineSlot: () => undefined,
        display: () => undefined,
        getConfig: () => ({ disableInitialLoad: false }),
        pubads: () => service,
      },
    });
    const batch = snapshotFirstDisplayBatchV1(fixture())!;
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      document: dom.window.document,
      projection: batch.projection,
      protocol: protocol(),
      setTimer: (callback) => callback,
      clearTimer: () => undefined,
    });
    startAdapter(adapter, {
      onBound: () => undefined,
      onFailure: () => undefined,
      onFirstAction: () => true,
      onRenderEnded: () => undefined,
    });
    listeners.get('slotRequested')?.({ slot });
    listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });

    sealing = true;
    expect(adapter.closeIngress(['slot-1'])).toBe(true);
    expect(adapter.captureHandoff()?.[0]?.[3].some(({ key }) => key === 'hb_pb')).toBe(false);
    expect(reentrantSealingCalls).toBe(1);
    expect(adapter.detachCommittedSlots(['slot-1'])).toBe(true);
    adapter.dispose();
    expect(targeting.get('hb_pb')).toEqual(['1.25']);
  });

  it('cleans failed TS slots while publisher targeting observation is still continuous', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div><div id="slot-2"></div>', {
      url: 'https://publisher.example/',
    });
    const targeting = new Map<string, string[]>([['hb_pb', ['publisher-original']]]);
    const publisherSlot = {
      clearTargeting: vi.fn((key: string) => targeting.delete(key)),
      getSlotElementId: () => 'slot-1',
      getTargeting: (key: string) => targeting.get(key) ?? [],
      setTargeting: vi.fn((key: string, value: string | string[]) => {
        targeting.set(key, Array.isArray(value) ? [...value] : [value]);
        return publisherSlot;
      }),
    };
    const failedSlot = {
      addService: () => failedSlot,
      getSlotElementId: () => 'slot-2',
      setTargeting: () => failedSlot,
    };
    const listeners = new Map<string, (event: unknown) => void>();
    const destroySlots = vi.fn((slots: object[]) => {
      expect(slots).toEqual([failedSlot]);
      slots.length = 0;
      publisherSlot.setTargeting('hb_pb', '1.25');
      return true;
    });
    const service = {
      addEventListener: (name: string, listener: (event: unknown) => void) =>
        listeners.set(name, listener),
      getSlots: () => [publisherSlot],
      refresh: () => undefined,
      removeEventListener: () => undefined,
    };
    Object.defineProperty(dom.window, 'googletag', {
      configurable: true,
      value: {
        pubadsReady: true,
        cmd: { push: (command: () => void) => command() },
        defineSlot: () => failedSlot,
        destroySlots,
        display: () => undefined,
        getConfig: () => ({ disableInitialLoad: false }),
        pubads: () => service,
      },
    });
    const batch = snapshotFirstDisplayBatchV1(twoSlotFixture())!;
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      document: dom.window.document,
      projection: batch.projection,
      protocol: protocol(),
      setTimer: (callback) => callback,
      clearTimer: () => undefined,
    });
    startAdapter(adapter, {
      onBound: () => undefined,
      onFailure: () => undefined,
      onFirstAction: () => true,
      onRenderEnded: () => undefined,
    });
    for (const slot of [publisherSlot, failedSlot]) {
      listeners.get('slotRequested')?.({ slot });
      listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });
    }

    expect(adapter.closeIngress(['slot-1'])).toBe(true);
    expect(destroySlots).toHaveBeenCalledOnce();
    expect(adapter.captureHandoff()?.[0]?.[3].some(({ key }) => key === 'hb_pb')).toBe(false);
    expect(adapter.detachCommittedSlots(['slot-1'])).toBe(true);
    adapter.dispose();
    expect(destroySlots).toHaveBeenCalledOnce();
    expect(targeting.get('hb_pb')).toEqual(['1.25']);
  });

  it('cleans failed publisher targeting before handoff with exact nested-write attribution', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div><div id="slot-2"></div>', {
      url: 'https://publisher.example/',
    });
    const acceptedTargeting = new Map<string, string[]>([['hb_pb', ['accepted-original']]]);
    const failedTargeting = new Map<string, string[]>([['hb_pb', ['failed-original']]]);
    let cleanupPhase = false;
    let reentrantCleanupCalls = 0;
    const acceptedSlot = {
      clearTargeting: vi.fn((key: string) => acceptedTargeting.delete(key)),
      getSlotElementId: () => 'slot-1',
      getTargeting: (key: string) => acceptedTargeting.get(key) ?? [],
      setTargeting: vi.fn((key: string, value: string | string[]) => {
        acceptedTargeting.set(key, Array.isArray(value) ? [...value] : [value]);
        return acceptedSlot;
      }),
    };
    const reenterAcceptedSlot = () => {
      if (!cleanupPhase || reentrantCleanupCalls > 0) return;
      reentrantCleanupCalls += 1;
      acceptedSlot.setTargeting('hb_pb', '1.25');
    };
    const failedSlot = {
      clearTargeting: vi.fn((key: string) => {
        failedTargeting.delete(key);
        reenterAcceptedSlot();
      }),
      getSlotElementId: () => 'slot-2',
      getTargeting: (key: string) => failedTargeting.get(key) ?? [],
      setTargeting: vi.fn((key: string, value: string | string[]) => {
        failedTargeting.set(key, Array.isArray(value) ? [...value] : [value]);
        reenterAcceptedSlot();
        return failedSlot;
      }),
    };
    const listeners = new Map<string, (event: unknown) => void>();
    const service = {
      addEventListener: (name: string, listener: (event: unknown) => void) =>
        listeners.set(name, listener),
      getSlots: () => [acceptedSlot, failedSlot],
      refresh: () => undefined,
      removeEventListener: () => undefined,
    };
    Object.defineProperty(dom.window, 'googletag', {
      configurable: true,
      value: {
        pubadsReady: true,
        cmd: { push: (command: () => void) => command() },
        defineSlot: () => undefined,
        display: () => undefined,
        getConfig: () => ({ disableInitialLoad: false }),
        pubads: () => service,
      },
    });
    const batch = snapshotFirstDisplayBatchV1(twoSlotFixture())!;
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      document: dom.window.document,
      projection: batch.projection,
      protocol: protocol(),
      setTimer: (callback) => callback,
      clearTimer: () => undefined,
    });
    startAdapter(adapter, {
      onBound: () => undefined,
      onFailure: () => undefined,
      onFirstAction: () => true,
      onRenderEnded: () => undefined,
    });
    for (const slot of [acceptedSlot, failedSlot]) {
      listeners.get('slotRequested')?.({ slot });
      listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });
    }

    cleanupPhase = true;
    expect(adapter.closeIngress(['slot-1'])).toBe(true);
    expect(failedTargeting.get('hb_pb')).toEqual(['failed-original']);
    expect(reentrantCleanupCalls).toBe(1);
    expect(adapter.captureHandoff()?.[0]?.[3].some(({ key }) => key === 'hb_pb')).toBe(false);
    const cleanupWrites =
      failedSlot.clearTargeting.mock.calls.length + failedSlot.setTargeting.mock.calls.length;

    expect(adapter.detachCommittedSlots(['slot-1'])).toBe(true);
    adapter.dispose();
    expect(reentrantCleanupCalls).toBe(1);
    expect(
      failedSlot.clearTargeting.mock.calls.length + failedSlot.setTargeting.mock.calls.length
    ).toBe(cleanupWrites);
    expect(acceptedTargeting.get('hb_pb')).toEqual(['1.25']);
  });

  it('marks refresh as the first request when initial load is disabled', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
      url: 'https://publisher.example/',
    });
    const events: string[] = [];
    const slot = {
      addService: () => slot,
      getSlotElementId: () => 'slot-1',
      setTargeting: () => slot,
    };
    const service = {
      addEventListener: (name: string) => events.push(`listen:${name}`),
      getSlots: () => [],
      refresh: () => events.push('refresh'),
      removeEventListener: () => undefined,
    };
    Object.defineProperty(dom.window, 'googletag', {
      configurable: true,
      value: {
        pubadsReady: true,
        cmd: { push: (command: () => void) => command() },
        defineSlot: () => slot,
        display: () => events.push('display'),
        getConfig: () => ({ disableInitialLoad: true }),
        pubads: () => service,
      },
    });
    const batch = snapshotFirstDisplayBatchV1(fixture())!;
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      document: dom.window.document,
      projection: batch.projection,
      protocol: protocol(),
      setTimer: (callback) => callback,
      clearTimer: () => undefined,
    });
    startAdapter(adapter, {
      onBound: () => undefined,
      onFailure: () => undefined,
      onFirstAction: () => {
        events.push('first-action');
        return true;
      },
      onRenderEnded: () => undefined,
    });
    expect(events).toEqual([
      'listen:slotRequested',
      'listen:slotRenderEnded',
      'display',
      'first-action',
      'refresh',
    ]);
  });

  it('removes a timed-out command so it cannot act after the readiness deadline', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
      url: 'https://publisher.example/',
    });
    const batch = snapshotFirstDisplayBatchV1(fixture())!;
    const timers: Array<() => void> = [];
    const failures: unknown[] = [];
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      document: dom.window.document,
      projection: batch.projection,
      protocol: protocol(),
      setTimer: (callback) => {
        timers.push(callback);
        return callback;
      },
      clearTimer: () => undefined,
    });
    startAdapter(adapter, {
      onBound: () => undefined,
      onFailure: (slotId, reason) => failures.push([slotId, reason]),
      onFirstAction: () => true,
      onRenderEnded: () => undefined,
    });
    const binding = (dom.window as unknown as { googletag: { cmd: Array<() => void> } }).googletag;
    expect(binding.cmd).toHaveLength(1);

    timers[0]?.();
    expect(failures).toEqual([['slot-1', 'external_ready_timeout']]);
    expect(binding.cmd).toHaveLength(0);
  });

  it('requires an attributable request before render completion and bounds request start', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
      url: 'https://publisher.example/',
    });
    const listeners = new Map<string, (event: unknown) => void>();
    const timers: Array<() => void> = [];
    const failures: unknown[] = [];
    const renders: unknown[] = [];
    const slot = {
      addService: () => slot,
      getSlotElementId: () => 'slot-1',
      setTargeting: () => slot,
    };
    const service = {
      addEventListener: (name: string, listener: (event: unknown) => void) =>
        listeners.set(name, listener),
      getSlots: () => [],
      removeEventListener: () => undefined,
    };
    Object.defineProperty(dom.window, 'googletag', {
      configurable: true,
      value: {
        pubadsReady: true,
        cmd: { push: (command: () => void) => command() },
        defineSlot: () => slot,
        display: () => undefined,
        getConfig: () => ({ disableInitialLoad: false }),
        pubads: () => service,
      },
    });
    const batch = snapshotFirstDisplayBatchV1(fixture())!;
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      document: dom.window.document,
      projection: batch.projection,
      protocol: protocol(),
      setTimer: (callback) => {
        timers.push(callback);
        return callback;
      },
      clearTimer: (handle) => {
        const index = timers.indexOf(handle as () => void);
        if (index >= 0) timers.splice(index, 1);
      },
    });
    startAdapter(adapter, {
      onBound: () => undefined,
      onFailure: (slotId, reason) => failures.push([slotId, reason]),
      onFirstAction: () => true,
      onRenderEnded: (cycle, result) => renders.push([cycle[6], result]),
    });

    listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });
    expect(renders).toEqual([]);
    expect(timers).toHaveLength(2);
    timers[0]?.();
    expect(failures).toEqual([['slot-1', 'gpt_request_timeout']]);
    listeners.get('slotRequested')?.({ slot });
    listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });
    expect(renders).toEqual([]);
  });

  it('uses the unique hydrated element id for slot definition and display', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1-hydrated"></div>', {
      url: 'https://publisher.example/',
    });
    const listeners = new Map<string, (event: unknown) => void>();
    const slot = {
      addService: () => slot,
      getSlotElementId: () => 'slot-1-hydrated',
      setTargeting: () => slot,
    };
    const defineSlot = vi.fn(() => slot);
    const display = vi.fn();
    Object.defineProperty(dom.window, 'googletag', {
      configurable: true,
      value: {
        pubadsReady: true,
        cmd: { push: (command: () => void) => command() },
        defineSlot,
        display,
        getConfig: () => ({ disableInitialLoad: false }),
        pubads: () => ({
          addEventListener: (name: string, listener: (event: unknown) => void) =>
            listeners.set(name, listener),
          getSlots: () => [],
          removeEventListener: () => undefined,
        }),
      },
    });
    const batch = snapshotFirstDisplayBatchV1(fixture())!;
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      document: dom.window.document,
      projection: batch.projection,
      protocol: protocol(),
      setTimer: (callback) => callback,
      clearTimer: () => undefined,
    });
    startAdapter(adapter, {
      onBound: () => undefined,
      onFailure: () => undefined,
      onFirstAction: () => true,
      onRenderEnded: () => undefined,
    });

    expect(defineSlot).toHaveBeenCalledWith('/123/example', [[300, 250]], 'slot-1-hydrated');
    expect(display).toHaveBeenCalledWith('slot-1-hydrated');
  });

  it('captures exact terminal slots, closes ingress, and detaches only committed identities', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
      url: 'https://publisher.example/',
    });
    const listeners = new Map<string, (event: unknown) => void>();
    const slot = {
      addService: () => slot,
      getSlotElementId: () => 'slot-1',
      setTargeting: () => slot,
    };
    const destroySlots = vi.fn(() => true);
    const service = {
      addEventListener: (name: string, listener: (event: unknown) => void) =>
        listeners.set(name, listener),
      getSlots: () => [],
      removeEventListener: vi.fn(),
    };
    Object.defineProperty(dom.window, 'googletag', {
      configurable: true,
      value: {
        pubadsReady: true,
        cmd: { push: (command: () => void) => command() },
        defineSlot: () => slot,
        destroySlots,
        display: () => undefined,
        getConfig: () => ({ disableInitialLoad: false }),
        pubads: () => service,
      },
    });
    const value = snapshotFirstDisplayBatchV1(fixture())!;
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      document: dom.window.document,
      projection: value.projection,
      protocol: protocol(),
      setTimer: (callback) => callback,
      clearTimer: () => undefined,
    });
    startAdapter(adapter, {
      onBound: () => undefined,
      onFailure: () => undefined,
      onFirstAction: () => true,
      onRenderEnded: () => undefined,
    });
    listeners.get('slotRequested')?.({ slot });
    listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });

    expect(adapter.closeIngress(['slot-1'])).toBe(true);
    expect(adapter.captureHandoff()).toEqual([expect.arrayContaining(['slot-1', slot])]);
    expect(adapter.detachCommittedSlots(['slot-1'])).toBe(true);
    expect(adapter.detachCommittedSlots(['slot-1'])).toBe(false);
    adapter.dispose();

    expect(service.removeEventListener).toHaveBeenCalledTimes(2);
    expect(destroySlots).not.toHaveBeenCalled();
  });

  it('captures six exact normalized diagnostics facts with stable slot identity', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
      url: 'https://publisher.example/',
    });
    vi.spyOn(dom.window.performance, 'now').mockReturnValue(12.5);
    const listeners = new Map<string, (event: unknown) => void>();
    const slot = {
      addService: () => slot,
      getAdUnitPath: () => '/123/example',
      getSlotElementId: () => 'slot-1',
      setTargeting: () => slot,
    };
    const service = {
      addEventListener: (name: string, listener: (event: unknown) => void) =>
        listeners.set(name, listener),
      getSlots: () => [],
      removeEventListener: () => undefined,
    };
    Object.defineProperty(dom.window, 'googletag', {
      configurable: true,
      value: {
        pubadsReady: true,
        cmd: { push: (command: () => void) => command() },
        defineSlot: () => slot,
        display: () => undefined,
        getConfig: () => ({ disableInitialLoad: false }),
        pubads: () => service,
      },
    });
    const value = snapshotFirstDisplayBatchV1(fixture())!;
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      clearTimer: () => undefined,
      diagnosticsActive: true,
      document: dom.window.document,
      projection: value.projection,
      protocol: protocol(),
      setTimer: (callback) => callback,
    });
    startAdapter(adapter, {
      onBound: () => undefined,
      onFailure: () => undefined,
      onFirstAction: () => true,
      onRenderEnded: () => undefined,
    });

    listeners.get('slotRequested')?.({ slot, responseIdentifier: 'response-one' });
    listeners.get('slotResponseReceived')?.({ slot, responseIdentifier: 'response-one' });
    listeners.get('slotRenderEnded')?.({
      slot,
      responseIdentifier: 'response-one',
      isEmpty: false,
      size: [300, 250],
      isBackfill: false,
      slotContentChanged: true,
    });
    listeners.get('slotOnload')?.({ slot });
    listeners.get('impressionViewable')?.({ slot });
    listeners.get('slotVisibilityChanged')?.({ slot, inViewPercentage: 42 });
    expect(adapter.closeIngress(['slot-1'])).toBe(true);

    const diagnostics = adapter.captureDiagnosticsHandoff();
    expect([...listeners.keys()]).toEqual([
      'slotRequested',
      'slotRenderEnded',
      'slotResponseReceived',
      'slotOnload',
      'impressionViewable',
      'slotVisibilityChanged',
    ]);
    expect(diagnostics?.[2]).toBe(2);
    expect(diagnostics?.[3]).toBe(0);
    expect(diagnostics?.[4]).toBe(0);
    expect(diagnostics?.[1]).toHaveLength(6);
    expect(diagnostics?.[0]).toEqual([
      [
        'slot-1',
        'gt1_1',
        2,
        false,
        [],
        [
          [
            1,
            'response-one',
            [
              'slotRequested',
              'slotResponseReceived',
              'slotRenderEnded',
              'slotOnload',
              'impressionViewable',
              'slotVisibilityChanged',
            ],
            'completed',
          ],
        ],
      ],
    ]);
    expect(diagnostics?.[1][0]).toEqual({
      version: 1,
      event: 'slotRequested',
      token: 'gt1_1',
      runtimeSlotNumber: 1,
      cycleOrdinal: 1,
      disposition: 'matched',
      issueReason: null,
      capturedAtMs: 12.5,
      elementId: 'slot-1',
      adUnitPath: '/123/example',
      requestedSlotSizes: [[300, 250]],
      isEmpty: null,
      renderedSize: null,
      isBackfill: null,
      slotContentChanged: null,
      visibilityPercent: null,
    });
    expect(diagnostics?.[1][2]).toMatchObject({
      event: 'slotRenderEnded',
      requestedSlotSizes: null,
      isEmpty: false,
      renderedSize: [300, 250],
      isBackfill: false,
      slotContentChanged: true,
    });
    expect(diagnostics?.[1][5]).toMatchObject({
      event: 'slotVisibilityChanged',
      visibilityPercent: 42,
    });
  });
});
