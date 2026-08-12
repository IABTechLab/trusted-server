import { describe, expect, it, vi } from 'vitest';

import {
  GptDiagnosticsDataApiController,
  type GptDiagnosticsPresentationControls,
} from '../../../src/integrations/gpt_diagnostics/data_api';
import { GptDiagnosticsStore } from '../../../src/integrations/gpt_diagnostics/store';

function controls(overrides: Partial<GptDiagnosticsPresentationControls> = {}) {
  return Object.freeze({
    dispose: vi.fn(),
    download: vi.fn(),
    exportBinding: vi.fn(() => Object.freeze({ status: 'bound' as const })),
    hide: vi.fn(),
    show: vi.fn(),
    subscribe: vi.fn(() => vi.fn()),
    ...overrides,
  });
}

function controller(store = new GptDiagnosticsStore()) {
  return new GptDiagnosticsDataApiController(store, {
    location: { origin: 'https://publisher.example', pathname: '/article' },
    schedule: (callback) => {
      callback();
      return () => undefined;
    },
  });
}

describe('critical GPT diagnostics data API', () => {
  it('deeply isolates delivery evidence and exports attribution issues', () => {
    const store = new GptDiagnosticsStore({ schedule: (callback) => callback() });
    const slot = Object.freeze({
      getSlotElementId: () => 'delivery-slot',
      getAdUnitPath: () => '/example/delivery-slot',
    });
    store.recordSlotRequested(slot, 1);
    store.recordSlotRenderEnded(
      slot,
      {
        isEmpty: false,
        adManager: {
          yieldGroupIds: [10],
          companyIds: [20],
        },
      },
      2
    );
    store.recordTrustedServerCreativeRequest('unknown-auction-slot');
    const target = controller(store);

    const snapshot = target.api.snapshot();
    const cycle = snapshot.slots[0]?.requests[0];

    expect(cycle?.adManager).toEqual({ yieldGroupIds: [10], companyIds: [20] });
    expect(Object.isFrozen(cycle?.adManager)).toBe(true);
    expect(Object.isFrozen(cycle?.adManager?.yieldGroupIds)).toBe(true);
    expect(Object.isFrozen(cycle?.adManager?.companyIds)).toBe(true);
    expect(snapshot.attributionIssues).toEqual([
      expect.objectContaining({ reason: 'creative_request_without_slot' }),
    ]);
    expect(Object.isFrozen(snapshot.attributionIssues)).toBe(true);
    expect(Object.isFrozen(snapshot.attributionIssues?.[0])).toBe(true);
    target.destroy();
  });

  it('coalesces exactly zero, one, and two committed updates to the latest snapshot', () => {
    const tasks: Array<() => void> = [];
    const store = new GptDiagnosticsStore({ schedule: (callback) => callback() });
    const target = new GptDiagnosticsDataApiController(store, {
      location: { origin: 'https://publisher.example', pathname: '/article' },
      schedule: (callback) => {
        tasks.push(callback);
        return () => undefined;
      },
    });
    const listener = vi.fn();
    target.api.subscribe(listener);
    const slot = Object.freeze({
      getSlotElementId: () => 'coalesced-slot',
      getAdUnitPath: () => '/example/coalesced-slot',
    });

    expect(tasks).toEqual([]);
    store.recordSlotRequested(slot);
    expect(tasks).toHaveLength(1);
    tasks.shift()?.();
    expect(listener).toHaveBeenCalledOnce();

    store.recordSlotVisibilityChanged(slot, 10);
    store.recordSlotVisibilityChanged(slot, 20);
    expect(tasks).toHaveLength(1);
    tasks.shift()?.();
    expect(listener).toHaveBeenCalledTimes(2);
    expect(listener.mock.calls[1]?.[0]).toEqual(
      expect.objectContaining({
        slots: [expect.objectContaining({ currentVisibilityPercentage: 20 })],
      })
    );
    target.destroy();
  });

  it('captures subscriber ids at commit and suppresses an id unsubscribed before delivery', () => {
    const tasks: Array<() => void> = [];
    const store = new GptDiagnosticsStore({ schedule: (callback) => callback() });
    const target = new GptDiagnosticsDataApiController(store, {
      location: { origin: 'https://publisher.example', pathname: '/article' },
      schedule: (callback) => {
        tasks.push(callback);
        return () => undefined;
      },
    });
    const first = vi.fn();
    const second = vi.fn();
    const releaseFirst = target.api.subscribe(first);
    const slot = Object.freeze({ getSlotElementId: () => 'captured-id-slot' });

    store.recordSlotRequested(slot);
    target.api.subscribe(second);
    releaseFirst();
    tasks.shift()?.();
    expect(first).not.toHaveBeenCalled();
    expect(second).not.toHaveBeenCalled();

    store.recordSlotVisibilityChanged(slot, 25);
    tasks.shift()?.();
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledOnce();
    target.destroy();
  });

  it('keeps a slow listener and its reentrant commit on separate notifier task stacks', () => {
    const tasks: Array<() => void> = [];
    const store = new GptDiagnosticsStore({ schedule: (callback) => callback() });
    const target = new GptDiagnosticsDataApiController(store, {
      location: { origin: 'https://publisher.example', pathname: '/article' },
      schedule: (callback) => {
        tasks.push(callback);
        return () => undefined;
      },
    });
    const slot = Object.freeze({ getSlotElementId: () => 'slow-listener-slot' });
    let listenerDepth = 0;
    let maximumDepth = 0;
    const slow = vi.fn(() => {
      listenerDepth += 1;
      maximumDepth = Math.max(maximumDepth, listenerDepth);
      if (slow.mock.calls.length === 1) store.recordSlotVisibilityChanged(slot, 50);
      listenerDepth -= 1;
    });
    const peer = vi.fn();
    target.api.subscribe(slow);
    target.api.subscribe(peer);

    store.recordSlotRequested(slot);
    expect(slow).not.toHaveBeenCalled();
    expect(peer).not.toHaveBeenCalled();
    tasks.shift()?.();
    expect(slow).toHaveBeenCalledOnce();
    expect(peer).toHaveBeenCalledOnce();
    expect(tasks).toHaveLength(1);
    tasks.shift()?.();
    expect(slow).toHaveBeenCalledTimes(2);
    expect(peer).toHaveBeenCalledTimes(2);
    expect(maximumDepth).toBe(1);
    target.destroy();
  });

  it('keeps public identity stable and does not spend public subscriber capacity on presentation', () => {
    const target = controller();
    const api = target.api;
    const presentation = controls();
    const factory = vi.fn((source, attachedApi) => {
      expect(source).toEqual(
        expect.objectContaining({
          bindingInputs: expect.any(Function),
          snapshot: expect.any(Function),
          subscribe: expect.any(Function),
        })
      );
      expect(attachedApi).toBe(api);
      return presentation;
    });

    const detach = target.attachPresentation(factory);
    const publicReleases = Array.from({ length: 32 }, () => api.subscribe(vi.fn()));

    expect(() => api.subscribe(vi.fn())).toThrow(
      expect.objectContaining({ code: 'subscriber_capacity', surface: 'gpt' })
    );
    expect(target.api).toBe(api);
    detach();
    detach();
    expect(presentation.subscribe).toHaveBeenCalledOnce();
    expect(vi.mocked(presentation.subscribe).mock.results[0]?.value).toHaveBeenCalledOnce();
    expect(presentation.dispose).toHaveBeenCalledOnce();
    expect(target.api).toBe(api);
    publicReleases.forEach((release) => release());
    target.destroy();
  });

  it('validates callability before state and rejects reentrant attachment without losing the outer owner', () => {
    const target = controller();
    const presentation = controls();
    const nested = vi.fn();
    const detach = target.attachPresentation(() => {
      expect(() => target.attachPresentation(nested)).toThrow(
        'GPT diagnostics presentation is unavailable'
      );
      return presentation;
    });

    expect(nested).not.toHaveBeenCalled();
    expect(() => target.attachPresentation(null as never)).toThrow(
      'GPT diagnostics presentation factory must be callable'
    );
    expect(() => target.attachPresentation(() => controls())).toThrow(
      'GPT diagnostics presentation is unavailable'
    );
    detach();
    expect(() => target.attachPresentation(() => controls())).not.toThrow();
    target.destroy();
  });

  it.each([
    [
      'malformed controls',
      () => Object.freeze({ dispose: vi.fn() }),
      'GPT diagnostics presentation controls are malformed',
    ],
    [
      'invalid subscription disposer',
      () => controls({ subscribe: vi.fn(() => undefined as never) }),
      'GPT diagnostics presentation disposer is unavailable',
    ],
    [
      'throwing subscription',
      () =>
        controls({
          subscribe: vi.fn(() => {
            throw new Error('subscription failed');
          }),
        }),
      'subscription failed',
    ],
  ])('rolls back %s without publishing presentation ownership', (_name, create, message) => {
    const target = controller();
    const presentation = create();

    expect(() => target.attachPresentation(() => presentation as never)).toThrow(message);
    expect(presentation.dispose).toHaveBeenCalledOnce();
    expect(() => target.attachPresentation(() => controls())).not.toThrow();
    target.destroy();
  });

  it('releases subscription and controls independently during detach and destroy', () => {
    const detachedDispose = vi.fn();
    const target = controller();
    const detach = target.attachPresentation(() =>
      controls({
        dispose: detachedDispose,
        subscribe: vi.fn(() => () => {
          throw new Error('hostile subscription release');
        }),
      })
    );

    expect(() => detach()).not.toThrow();
    expect(detachedDispose).toHaveBeenCalledOnce();

    const destroyedDispose = vi.fn();
    target.attachPresentation(() =>
      controls({
        dispose: destroyedDispose,
        subscribe: vi.fn(() => () => {
          throw new Error('hostile destroy release');
        }),
      })
    );
    expect(() => target.destroy()).not.toThrow();
    expect(destroyedDispose).toHaveBeenCalledOnce();
  });

  it.each(['throw', 'invalid disposer'] as const)(
    'contains a notifier scheduler %s and recovers on the next commit',
    (failure) => {
      const tasks: Array<() => void> = [];
      let attempts = 0;
      const store = new GptDiagnosticsStore({ schedule: (callback) => callback() });
      const target = new GptDiagnosticsDataApiController(store, {
        location: { origin: 'https://publisher.example', pathname: '/article' },
        schedule: (callback) => {
          attempts += 1;
          if (attempts === 1) {
            if (failure === 'throw') throw new Error('fictional scheduler failure');
            return undefined as never;
          }
          tasks.push(callback);
          return () => undefined;
        },
      });
      target.api.subscribe(() => {
        throw new Error('fictional subscriber failure');
      });
      const listener = vi.fn();
      target.api.subscribe(listener);
      const slot = Object.freeze({
        getSlotElementId: () => 'notifier-slot',
        getAdUnitPath: () => '/example/notifier-slot',
      });

      expect(() => store.recordSlotRequested(slot)).not.toThrow();
      expect(listener).not.toHaveBeenCalled();
      expect(() => store.recordSlotVisibilityChanged(slot, 25)).not.toThrow();
      expect(tasks).toHaveLength(1);
      expect(() => tasks.shift()?.()).not.toThrow();
      expect(listener).toHaveBeenCalledOnce();
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({
          slots: [expect.objectContaining({ currentVisibilityPercentage: 25 })],
        })
      );
      target.destroy();
    }
  );

  it('makes a retained scheduled notifier inert after controller destruction', () => {
    const tasks: Array<() => void> = [];
    const store = new GptDiagnosticsStore({ schedule: (callback) => callback() });
    const target = new GptDiagnosticsDataApiController(store, {
      location: { origin: 'https://publisher.example', pathname: '/article' },
      schedule: (callback) => {
        tasks.push(callback);
        return () => undefined;
      },
    });
    const listener = vi.fn();
    target.api.subscribe(listener);

    store.recordSlotRequested(Object.freeze({ getSlotElementId: () => 'stale-notifier-slot' }));
    expect(tasks).toHaveLength(1);
    target.destroy();
    expect(() => tasks.shift()?.()).not.toThrow();
    expect(listener).not.toHaveBeenCalled();
  });
});
