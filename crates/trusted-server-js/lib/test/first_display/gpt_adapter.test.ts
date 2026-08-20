import { describe, expect, it, vi } from 'vitest';
import { JSDOM } from 'jsdom';

import { createFirstDisplayGoogletagBatch } from '../../src/first_display/adapters/googletag';
import { snapshotFirstDisplayBatchV1 } from '../../src/first_display/leaf/projection';

const RESERVATION_ID = `r1_${'a'.repeat(22)}`;

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
    adapter.start({
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
    expect(adapter.captureDiagnosticsHandoff()?.cycles).toEqual([
      expect.objectContaining({
        nextCycleOrdinal: 3,
        records: [
          expect.objectContaining({ ordinal: 1, state: 'completed' }),
          expect.objectContaining({
            ordinal: 2,
            responseIdentifier: 'response-two',
            seen: ['slotRequested', 'slotRenderEnded'],
            state: 'completed',
          }),
        ],
      }),
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
      getSlots: vi.fn(() => []),
      refresh: vi.fn(() => events.push('refresh')),
      removeEventListener: vi.fn(),
    };
    Object.defineProperty(dom.window, 'googletag', {
      configurable: true,
      value: {
        cmd: { push: (command: () => void) => command() },
        defineSlot: vi.fn(() => slot),
        display: vi.fn(() => events.push('display')),
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
      adapter.start({
        onBound: ({ element, slotId, ownership }) => {
          expect(element).toBe(dom.window.document.getElementById('slot-1'));
          events.push(`bound:${slotId}:${ownership}`);
        },
        onFailure: (slotId, reason) => failures.push([slotId, reason]),
        onFirstAction: () => {
          events.push('first-action');
          return true;
        },
        onRenderEnded: (cycle, result) => renders.push([cycle.slotId, result]),
      })
    ).toBe(true);
    expect(events).toEqual([
      'bound:slot-1:trusted_server',
      'target:hb_adid',
      'target:hb_pb',
      'target:placement',
      'first-action',
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
    let bound: { readonly isCurrent: () => boolean } | undefined;
    const retired = vi.fn();
    const adapter = createFirstDisplayGoogletagBatch({
      browser: dom.window as unknown as Window,
      clearTimer: () => undefined,
      document: dom.window.document,
      projection: batch.projection,
      protocol: protocol(),
      setTimer: (callback) => callback,
    });
    adapter.start({
      onBound: (cycle) => {
        bound = cycle;
      },
      onFailure: () => undefined,
      onFirstAction: () => true,
      onRenderEnded: () => undefined,
      onRetire: retired,
    });

    expect(bound?.isCurrent()).toBe(true);
    listeners.get('slotRequested')?.({ slot });
    listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });
    expect(bound?.isCurrent()).toBe(true);

    expect(() => binding.destroySlots([slot])).toThrow('fictional destroy failure');
    expect(bound?.isCurrent()).toBe(true);
    expect(retired).not.toHaveBeenCalled();
    destroyMode = 'false';
    expect(binding.destroySlots([slot])).toBe(false);
    expect(bound?.isCurrent()).toBe(true);
    expect(retired).not.toHaveBeenCalled();
    destroyMode = 'true';
    const targets = [slot];
    expect(binding.destroySlots(targets)).toBe(true);
    expect(targets).toEqual([]);
    expect(binding.defineSlot('/publisher/replacement', [[300, 250]], 'slot-1')).toBe(replacement);
    binding.display('slot-1');
    expect(bound?.isCurrent()).toBe(false);
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
      adapter.start({
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

    adapter.start({
      onBound: ({ ownership }) => expect(ownership).toBe('publisher'),
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
    expect(adapter.captureHandoff()?.[0]?.targetingOwnership).toEqual([
      { installed: RESERVATION_ID, key: 'hb_adid', prior: [] },
      { installed: '1.25', key: 'hb_pb', prior: ['publisher-original'] },
    ]);
    adapter.dispose();
    expect(targeting.get('placement')).toEqual(['article']);
    expect(targeting.get('hb_pb')).toEqual(['publisher-original']);
    expect(targeting.has('hb_adid')).toBe(false);
  });

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
    adapter.start({
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
    expect(adapter.captureHandoff()?.[0]?.targetingOwnership).toEqual([]);
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
      adapter.start({
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
      adapter.start({
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
    adapter.start({
      onBound: () => undefined,
      onFailure: () => undefined,
      onFirstAction: () => true,
      onRenderEnded: () => undefined,
    });
    listeners.get('slotRequested')?.({ slot });
    listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });

    sealing = true;
    expect(adapter.closeIngress(['slot-1'])).toBe(true);
    expect(
      adapter.captureHandoff()?.[0]?.targetingOwnership.some(({ key }) => key === 'hb_pb')
    ).toBe(false);
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
    adapter.start({
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
    expect(
      adapter.captureHandoff()?.[0]?.targetingOwnership.some(({ key }) => key === 'hb_pb')
    ).toBe(false);
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
    adapter.start({
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
    expect(
      adapter.captureHandoff()?.[0]?.targetingOwnership.some(({ key }) => key === 'hb_pb')
    ).toBe(false);
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
    adapter.start({
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
    adapter.start({
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
    adapter.start({
      onBound: () => undefined,
      onFailure: (slotId, reason) => failures.push([slotId, reason]),
      onFirstAction: () => true,
      onRenderEnded: (cycle, result) => renders.push([cycle.slotId, result]),
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
    adapter.start({
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
    adapter.start({
      onBound: () => undefined,
      onFailure: () => undefined,
      onFirstAction: () => true,
      onRenderEnded: () => undefined,
    });
    listeners.get('slotRequested')?.({ slot });
    listeners.get('slotRenderEnded')?.({ slot, isEmpty: false });

    expect(adapter.closeIngress(['slot-1'])).toBe(true);
    expect(adapter.captureHandoff()).toEqual([
      expect.objectContaining({ slotId: 'slot-1', physicalSlot: slot }),
    ]);
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
    adapter.start({
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
    expect(diagnostics).toMatchObject({
      nextTraceTokenOrdinal: 2,
      overflowCount: 0,
      dropCount: 0,
    });
    expect(diagnostics?.facts).toHaveLength(6);
    expect(
      (
        diagnostics as unknown as {
          readonly cycles: readonly unknown[];
        }
      )?.cycles
    ).toEqual([
      {
        nextCycleOrdinal: 2,
        quarantines: [],
        records: [
          {
            ordinal: 1,
            responseIdentifier: 'response-one',
            seen: [
              'slotRequested',
              'slotResponseReceived',
              'slotRenderEnded',
              'slotOnload',
              'impressionViewable',
              'slotVisibilityChanged',
            ],
            state: 'completed',
          },
        ],
        slotId: 'slot-1',
        token: 'gt1_1',
        unknownPriorCycle: false,
      },
    ]);
    expect(diagnostics?.facts[0]).toEqual({
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
      isEmpty: null,
      renderedSize: null,
      isBackfill: null,
      slotContentChanged: null,
      visibilityPercent: null,
    });
    expect(diagnostics?.facts[2]).toMatchObject({
      event: 'slotRenderEnded',
      isEmpty: false,
      renderedSize: [300, 250],
      isBackfill: false,
      slotContentChanged: true,
    });
    expect(diagnostics?.facts[5]).toMatchObject({
      event: 'slotVisibilityChanged',
      visibilityPercent: 42,
    });
  });
});
