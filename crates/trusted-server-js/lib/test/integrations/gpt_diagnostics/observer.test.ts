import { describe, expect, it, vi } from 'vitest';

import {
  GptDiagnosticsObserver,
  type GptDiagnosticsObserverStore,
} from '../../../src/integrations/gpt_diagnostics/observer';
import type { GptDiagnosticsSlotLike } from '../../../src/integrations/gpt_diagnostics/store';

const EVENT_NAMES = [
  'slotRequested',
  'slotResponseReceived',
  'slotRenderEnded',
  'slotOnload',
  'impressionViewable',
  'slotVisibilityChanged',
] as const;

type EventName = (typeof EVENT_NAMES)[number];
type EventListener = (event: { slot: GptDiagnosticsSlotLike; [key: string]: unknown }) => void;

function fakeStore(): GptDiagnosticsObserverStore {
  return {
    markGptObserved: vi.fn(),
    recordSlotRequested: vi.fn(),
    recordSlotResponseReceived: vi.fn(),
    recordSlotRenderEnded: vi.fn(),
    recordSlotOnload: vi.fn(),
    recordImpressionViewable: vi.fn(),
    recordSlotVisibilityChanged: vi.fn(),
    recordPublisherRefresh: vi.fn(),
  };
}

function fakeSlot(): GptDiagnosticsSlotLike {
  return {
    getSlotElementId: () => 'ad-slot-example',
    getAdUnitPath: () => '/example/site/banner',
  };
}

function controlledGpt() {
  const listeners = new Map<EventName, EventListener[]>();
  const addEventListener = vi.fn((name: EventName, listener: EventListener) => {
    const current = listeners.get(name) ?? [];
    current.push(listener);
    listeners.set(name, current);
  });
  const pubads = {
    addEventListener,
    refresh: vi.fn(),
  };
  const display = vi.fn();
  const defineSlot = vi.fn();
  const cmd: Array<() => void> = [];
  const googletag = {
    cmd,
    pubads: () => pubads,
    display,
    defineSlot,
  };

  return {
    window: { googletag },
    googletag,
    pubads,
    listeners,
    emit(name: EventName, event: Parameters<EventListener>[0]) {
      for (const listener of listeners.get(name) ?? []) listener(event);
    },
  };
}

describe('GptDiagnosticsObserver', () => {
  it('installs exactly the six documented listeners through googletag.cmd', () => {
    const store = fakeStore();
    const gpt = controlledGpt();
    const observer = new GptDiagnosticsObserver(store, { window: gpt.window });

    observer.install();

    expect(gpt.googletag.cmd).toHaveLength(1);
    expect(gpt.pubads.addEventListener).not.toHaveBeenCalled();

    gpt.googletag.cmd[0]();

    expect(gpt.pubads.addEventListener).toHaveBeenCalledTimes(EVENT_NAMES.length);
    expect(gpt.pubads.addEventListener.mock.calls.map(([name]) => name)).toEqual(EVENT_NAMES);
    expect(store.markGptObserved).toHaveBeenCalledTimes(1);
  });

  it('is idempotent before and after command queue execution', () => {
    const store = fakeStore();
    const gpt = controlledGpt();
    const observer = new GptDiagnosticsObserver(store, { window: gpt.window });

    observer.install();
    observer.install();
    expect(gpt.googletag.cmd).toHaveLength(1);

    gpt.googletag.cmd[0]();
    observer.install();
    gpt.googletag.cmd[0]();

    expect(gpt.pubads.addEventListener).toHaveBeenCalledTimes(EVENT_NAMES.length);
    expect(store.markGptObserved).toHaveBeenCalledTimes(1);
  });

  it('observes publisher refresh slots without changing the delegated call', () => {
    const store = fakeStore();
    const gpt = controlledGpt();
    const slot = fakeSlot();
    const receiver = { refresh: gpt.pubads.refresh };
    const originalRefresh = vi.fn(function (this: unknown, ...args: unknown[]) {
      return { receiver: this, args };
    });
    gpt.pubads.refresh = originalRefresh;
    const observer = new GptDiagnosticsObserver(store, { window: gpt.window });
    observer.install();
    gpt.googletag.cmd[0]();

    const result = Reflect.apply(gpt.pubads.refresh, receiver, [
      [slot],
      { changeCorrelator: false },
    ]);

    expect(store.recordPublisherRefresh).toHaveBeenCalledWith([slot]);
    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(result).toEqual({ receiver, args: [[slot], { changeCorrelator: false }] });
  });

  it('creates a command queue and waits when GPT is absent', () => {
    const store = fakeStore();
    const delayedWindow: {
      googletag?: {
        cmd: Array<() => void>;
        pubads?: () => { addEventListener: (name: EventName, listener: EventListener) => void };
      };
    } = {};
    const observer = new GptDiagnosticsObserver(store, { window: delayedWindow });

    observer.install();

    expect(delayedWindow.googletag?.cmd).toHaveLength(1);
    const gpt = controlledGpt();
    delayedWindow.googletag!.pubads = gpt.googletag.pubads;
    delayedWindow.googletag!.cmd[0]();

    expect(gpt.pubads.addEventListener).toHaveBeenCalledTimes(EVENT_NAMES.length);
  });

  it('preserves an already-loaded custom command push contract', () => {
    const store = fakeStore();
    const gpt = controlledGpt();
    const callbacks: Array<() => void> = [];
    const customPush = vi.fn((...next: Array<() => void>) => {
      callbacks.push(...next);
      for (const callback of next) callback();
      return callbacks.length;
    });
    const observer = new GptDiagnosticsObserver(store, {
      window: {
        googletag: {
          cmd: { push: customPush },
          pubads: gpt.googletag.pubads,
        },
      },
    });

    observer.install();

    expect(customPush).toHaveBeenCalledTimes(1);
    expect(gpt.pubads.addEventListener).toHaveBeenCalledTimes(EVENT_NAMES.length);
  });

  it('normalizes allowed callback facts and forwards every event kind', () => {
    const store = fakeStore();
    const gpt = controlledGpt();
    const slot = fakeSlot();
    const observer = new GptDiagnosticsObserver(store, { window: gpt.window });
    observer.install();
    gpt.googletag.cmd[0]();

    gpt.emit('slotRequested', { slot });
    gpt.emit('slotResponseReceived', { slot });
    gpt.emit('slotRenderEnded', {
      slot,
      isEmpty: false,
      size: [300, 250],
      isBackfill: true,
      slotContentChanged: false,
      creativeId: 'must-not-pass-through',
    });
    gpt.emit('slotOnload', { slot });
    gpt.emit('impressionViewable', { slot });
    gpt.emit('slotVisibilityChanged', { slot, inViewPercentage: 42 });

    expect(store.recordSlotRequested).toHaveBeenCalledWith(slot);
    expect(store.recordSlotResponseReceived).toHaveBeenCalledWith(slot);
    expect(store.recordSlotRenderEnded).toHaveBeenCalledWith(slot, {
      isEmpty: false,
      size: [300, 250],
      isBackfill: true,
      slotContentChanged: false,
    });
    expect(store.recordSlotOnload).toHaveBeenCalledWith(slot);
    expect(store.recordImpressionViewable).toHaveBeenCalledWith(slot);
    expect(store.recordSlotVisibilityChanged).toHaveBeenCalledWith(slot, 42);
  });

  it('forwards the Ad Manager identifiers GPT reports for the delivered ad', () => {
    const store = fakeStore();
    const gpt = controlledGpt();
    const slot = fakeSlot();
    const observer = new GptDiagnosticsObserver(store, { window: gpt.window });
    observer.install();
    gpt.googletag.cmd[0]();

    gpt.emit('slotRenderEnded', {
      slot,
      isEmpty: false,
      lineItemId: 6543210987,
      creativeId: 1234567890,
      campaignId: 2345678901,
      advertiserId: 3456789012,
      sourceAgnosticLineItemId: 6543210987,
      yieldGroupIds: [11, 12],
      companyIds: [],
    });

    expect(store.recordSlotRenderEnded).toHaveBeenCalledWith(
      slot,
      expect.objectContaining({
        adManager: {
          lineItemId: 6543210987,
          creativeId: 1234567890,
          campaignId: 2345678901,
          advertiserId: 3456789012,
          sourceAgnosticLineItemId: 6543210987,
          yieldGroupIds: [11, 12],
        },
      })
    );
  });

  it('drops malformed Ad Manager identifiers instead of reporting them', () => {
    const store = fakeStore();
    const gpt = controlledGpt();
    const slot = fakeSlot();
    const observer = new GptDiagnosticsObserver(store, { window: gpt.window });
    observer.install();
    gpt.googletag.cmd[0]();

    gpt.emit('slotRenderEnded', {
      slot,
      isEmpty: false,
      lineItemId: null,
      creativeId: '1234567890',
      campaignId: 0,
      advertiserId: 1.5,
      yieldGroupIds: 'not-a-list',
    });

    expect(store.recordSlotRenderEnded).toHaveBeenCalledWith(
      slot,
      expect.objectContaining({ adManager: undefined })
    );
  });

  it('drops unsupported or invalid rendered sizes', () => {
    const store = fakeStore();
    const gpt = controlledGpt();
    const slot = fakeSlot();
    const observer = new GptDiagnosticsObserver(store, { window: gpt.window });
    observer.install();
    gpt.googletag.cmd[0]();

    gpt.emit('slotRenderEnded', { slot, isEmpty: false, size: 'fluid' });

    expect(store.recordSlotRenderEnded).toHaveBeenCalledWith(
      slot,
      expect.objectContaining({ size: undefined })
    );
  });

  it('contains callback and Slot accessor failures and warns', () => {
    const store = fakeStore();
    vi.mocked(store.recordSlotRequested).mockImplementation(() => {
      throw new Error('store failed');
    });
    const logger = { warn: vi.fn() };
    const gpt = controlledGpt();
    const observer = new GptDiagnosticsObserver(store, { window: gpt.window, logger });
    observer.install();
    gpt.googletag.cmd[0]();
    const event = {
      get slot(): GptDiagnosticsSlotLike {
        throw new Error('slot accessor failed');
      },
    };

    expect(() => gpt.emit('slotRequested', { slot: fakeSlot() })).not.toThrow();
    expect(() => gpt.emit('slotOnload', event)).not.toThrow();
    expect(logger.warn).toHaveBeenCalledTimes(2);
  });

  it('contains command queue and listener installation failures', () => {
    const store = fakeStore();
    const logger = { warn: vi.fn() };
    const queueObserver = new GptDiagnosticsObserver(store, {
      window: {
        googletag: {
          cmd: {
            push: () => {
              throw new Error('queue failed');
            },
          },
        },
      },
      logger,
    });

    expect(() => queueObserver.install()).not.toThrow();

    const gpt = controlledGpt();
    gpt.pubads.addEventListener.mockImplementation(() => {
      throw new Error('listener failed');
    });
    const listenerObserver = new GptDiagnosticsObserver(store, {
      window: gpt.window,
      logger,
    });
    listenerObserver.install();

    expect(() => gpt.googletag.cmd[0]()).not.toThrow();
    expect(logger.warn).toHaveBeenCalledTimes(2);
  });

  it('wraps only PubAds refresh and leaves unrelated GPT and browser methods intact', () => {
    const store = fakeStore();
    const gpt = controlledGpt();
    const observer = new GptDiagnosticsObserver(store, { window: gpt.window });
    const references = {
      display: gpt.googletag.display,
      defineSlot: gpt.googletag.defineSlot,
      refresh: gpt.pubads.refresh,
      fetch: window.fetch,
      XMLHttpRequest: window.XMLHttpRequest,
      pushState: window.history.pushState,
      replaceState: window.history.replaceState,
    };

    observer.install();
    gpt.googletag.cmd[0]();

    expect(gpt.googletag.display).toBe(references.display);
    expect(gpt.googletag.defineSlot).toBe(references.defineSlot);
    expect(gpt.pubads.refresh).not.toBe(references.refresh);
    expect(window.fetch).toBe(references.fetch);
    expect(window.XMLHttpRequest).toBe(references.XMLHttpRequest);
    expect(window.history.pushState).toBe(references.pushState);
    expect(window.history.replaceState).toBe(references.replaceState);
  });
});
