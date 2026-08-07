import { describe, expect, it, vi } from 'vitest';

import type {
  GoogletagAdapter,
  GoogletagPublisherCallObserver,
} from '../../../src/adapters/googletag';
import { createGptStartup } from '../../../src/integrations/gpt/startup';
import type { SlotService } from '../../../src/services/slots';

describe('GPT startup bridge', () => {
  it('installs one reversible typed observer and delegates all handoff state to slots', () => {
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
    }) satisfies Pick<
      SlotService,
      | 'claimPublisherGptSlot'
      | 'preparePublisherDisplay'
      | 'preparePublisherRefresh'
      | 'recordPublisherDestruction'
    >;
    const start = vi.fn();
    const startup = createGptStartup({ googletag: adapter, slots: () => slots, start });

    expect(startup.activate()).toBe(release);
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
    expect(start).toHaveBeenCalledExactlyOnceWith(config);
  });
});
