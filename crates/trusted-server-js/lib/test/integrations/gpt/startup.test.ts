import { describe, expect, it, vi } from 'vitest';

import {
  createBrowserGoogletagAdapter,
  type GoogletagAdapter,
  type GoogletagPublisherCallObserver,
} from '../../../src/adapters/googletag';
import { createGptStartup } from '../../../src/integrations/gpt/startup';
import { createSlotService, type SlotService } from '../../../src/services/slots';

describe('GPT startup bridge', () => {
  it('installs one reversible typed observer and delegates all handoff state to slots', () => {
    const order: string[] = [];
    let observer: GoogletagPublisherCallObserver | undefined;
    const release = vi.fn();
    const observePublisherCalls = vi.fn((candidate: GoogletagPublisherCallObserver) => {
      observer = candidate;
      return release;
    });
    const adapter = Object.freeze({ observePublisherCalls }) as unknown as GoogletagAdapter;
    const slot = {};
    const slots = Object.freeze({
      claimPublisherGptSlot: vi.fn(() => Object.freeze({ action: 'handoff' as const, slot })),
      preparePublisherDisplay: vi.fn(() => Object.freeze({ action: 'suppress' as const })),
      preparePublisherRefresh: vi.fn(() => Object.freeze({ action: 'suppress' as const })),
      recordPublisherDestruction: vi.fn(() => true),
      start: vi.fn(() => {
        order.push('slots:start');
        return Object.freeze({
          status: 'present' as const,
          result: Promise.resolve(),
          dispose: vi.fn(),
        });
      }),
    }) satisfies Pick<
      SlotService,
      | 'claimPublisherGptSlot'
      | 'preparePublisherDisplay'
      | 'preparePublisherRefresh'
      | 'recordPublisherDestruction'
      | 'start'
    >;
    const start = vi.fn(() => order.push('external:start'));
    const startup = createGptStartup({ googletag: adapter, slots: () => slots, start });

    expect(startup.activate()).toBe(release);
    expect(slots.start).not.toHaveBeenCalled();
    expect(observePublisherCalls).toHaveBeenCalledTimes(1);
    expect(
      observer?.defineSlot?.({
        adUnitPath: '/publisher',
        elementId: 'slot',
        initialLoadDisabled: true,
        sizes: [300, 250],
      })
    ).toEqual({ action: 'handoff', slot });
    expect(observer?.display?.({ initialLoadDisabled: true, target: 'slot' })).toEqual({
      action: 'suppress',
    });
    expect(
      observer?.refresh?.({ requestedSlots: undefined, slots: Object.freeze([slot]) })
    ).toEqual({ action: 'suppress' });
    observer?.destroySlots?.({ slots: Object.freeze([slot, {}]) });
    expect(slots.recordPublisherDestruction).toHaveBeenCalledTimes(2);

    const config = Object.freeze({ disableInitialLoad: true });
    startup.start(config);
    expect(slots.start).toHaveBeenCalledOnce();
    expect(start).toHaveBeenCalledExactlyOnceWith(config);
    expect(order).toEqual(['slots:start', 'external:start']);
  });

  it('keeps reversible activation timer-free and begins readiness only from start', () => {
    vi.useFakeTimers();
    const adapter = createBrowserGoogletagAdapter({});
    const slots = createSlotService({ googletag: adapter });
    const startup = createGptStartup({ googletag: adapter, slots: () => slots });

    const release = startup.activate();
    slots.activate();
    expect(vi.getTimerCount()).toBe(0);

    startup.start(Object.freeze({}));
    expect(vi.getTimerCount()).toBe(1);

    release();
    slots.dispose();
    adapter.dispose();
    expect(vi.getTimerCount()).toBe(0);
    vi.useRealTimers();
  });
});
