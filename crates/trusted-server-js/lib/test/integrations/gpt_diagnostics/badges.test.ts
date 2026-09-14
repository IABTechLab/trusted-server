import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { GptDiagnosticsBinding } from '../../../src/core/types';
import type { GptDiagnosticsBindingView } from '../../../src/integrations/gpt_diagnostics/binding';
import {
  formatGptDiagnosticsBadgeText,
  GptDiagnosticsBadgeManager,
} from '../../../src/integrations/gpt_diagnostics/badges';
import { GptDiagnosticsStore } from '../../../src/integrations/gpt_diagnostics/store';

class FakeBindings {
  private readonly listeners = new Set<() => void>();
  private readonly values = new Map<number, GptDiagnosticsBindingView>();

  get(runtimeSlotNumber: number): GptDiagnosticsBindingView {
    return (
      this.values.get(runtimeSlotNumber) ?? {
        binding: { status: 'unbound', reason: 'missing_element' },
        visible: false,
      }
    );
  }

  set(
    runtimeSlotNumber: number,
    binding: GptDiagnosticsBinding,
    element?: HTMLElement,
    visible = false
  ): void {
    this.values.set(runtimeSlotNumber, { binding, element, visible });
    for (const listener of this.listeners) listener();
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
}

function slot(id: string) {
  return {
    getSlotElementId: () => id,
    getAdUnitPath: () => `/example/site/${id}`,
  };
}

function rectangle(left: number, top: number, width: number, height: number): DOMRect {
  return {
    left,
    top,
    width,
    height,
    right: left + width,
    bottom: top + height,
    x: left,
    y: top,
    toJSON: () => ({}),
  } as DOMRect;
}

function runFrame(frames: Array<() => void>): void {
  const frame = frames.shift();
  if (!frame) throw new Error('Missing scheduled frame');
  frame();
}

const gptDiagnosticsBadgeTextForTest = formatGptDiagnosticsBadgeText;

function queueFrame(frames: Array<() => void>): (callback: () => void) => () => void {
  return (callback) => {
    frames.push(callback);
    return () => {
      const index = frames.indexOf(callback);
      if (index >= 0) frames.splice(index, 1);
    };
  };
}

beforeEach(() => {
  document.body.replaceChildren();
  Object.defineProperty(window, 'innerWidth', { configurable: true, value: 1024 });
  Object.defineProperty(window, 'innerHeight', { configurable: true, value: 768 });
});

afterEach(() => {
  vi.restoreAllMocks();
  document.body.replaceChildren();
});

describe('GptDiagnosticsBadgeManager', () => {
  it('shows badges only for requested, uniquely bound, visible, non-zero slots', () => {
    const frames: Array<() => void> = [];
    const store = new GptDiagnosticsStore({ schedule: (callback) => callback() });
    const bindings = new FakeBindings();
    const eligible = document.createElement('div');
    const zeroSize = document.createElement('div');
    const offscreen = document.createElement('div');
    const neverRequested = document.createElement('div');
    document.body.append(eligible, zeroSize, offscreen, neverRequested);
    vi.spyOn(eligible, 'getBoundingClientRect').mockReturnValue(rectangle(100, 100, 300, 250));
    vi.spyOn(zeroSize, 'getBoundingClientRect').mockReturnValue(rectangle(0, 0, 0, 0));
    vi.spyOn(offscreen, 'getBoundingClientRect').mockReturnValue(rectangle(100, 900, 300, 250));
    vi.spyOn(neverRequested, 'getBoundingClientRect').mockReturnValue(
      rectangle(100, 100, 300, 250)
    );

    store.recordSlotRequested(slot('eligible'));
    store.recordSlotRequested(slot('zero'));
    store.recordSlotRequested(slot('offscreen'));
    store.recordSlotVisibilityChanged(slot('unbound'), 10);
    bindings.set(1, { status: 'bound' }, eligible, true);
    bindings.set(2, { status: 'bound' }, zeroSize, true);
    bindings.set(3, { status: 'bound' }, offscreen, false);
    bindings.set(4, { status: 'ambiguous', reason: 'duplicate_dom_id' });
    bindings.set(5, { status: 'bound' }, neverRequested, true);

    const layer = document.createElement('div');
    document.body.append(layer);
    const manager = new GptDiagnosticsBadgeManager(store, bindings, {
      scheduleFrame: queueFrame(frames),
    });
    manager.setLayer(layer);
    runFrame(frames);

    expect(layer.querySelectorAll('.tsgd-badge')).toHaveLength(1);
    expect(layer.querySelector<HTMLElement>('.tsgd-badge')?.dataset.runtimeSlot).toBe('1');
    manager.destroy();
  });

  it('labels the delivery state the store derived rather than raw timestamps', () => {
    // The store owns the delivery ladder; a badge that re-derived it from these
    // timestamps could contradict the panel and the export.
    const pendingBadge = gptDiagnosticsBadgeTextForTest({
      requestNumber: 1,
      isEmpty: false,
      incompleteSequence: false,
      durations: {},
      requestPath: 'competing',
      trustedServerOpportunity: 'renderable_candidate',
      trustedServerCreativeRequestAtMs: 10,
      trustedServerCreativeResponseAtMs: 11,
      delivery: 'pending',
    });
    expect(pendingBadge).toBe('Filled · TS candidate (pending) · Competing paths');

    const unconfirmedBadge = gptDiagnosticsBadgeTextForTest({
      requestNumber: 1,
      isEmpty: false,
      incompleteSequence: false,
      durations: {},
      requestPath: 'trusted_server_direct',
      trustedServerOpportunity: 'unrenderable_candidate',
      trustedServerCreativeRequestAtMs: 10,
      delivery: 'candidate_unconfirmed',
    });
    expect(unconfirmedBadge).toBe('Filled · TS unconfirmed');

    const emptyBadge = gptDiagnosticsBadgeTextForTest({
      requestNumber: 1,
      isEmpty: true,
      incompleteSequence: false,
      durations: {},
      trustedServerCreativeRequestAtMs: 10,
      trustedServerCreativeResponseAtMs: 11,
      delivery: 'not_applicable',
    });
    expect(emptyBadge, 'an empty cycle must not claim a Trusted Server response').toBe('Empty');
  });

  it('renders compact evidence-safe labels for every delivery state', () => {
    const responseSentBadge = gptDiagnosticsBadgeTextForTest({
      requestNumber: 1,
      isEmpty: false,
      incompleteSequence: false,
      durations: {},
      delivery: 'trusted_server_response_sent',
    });
    expect(responseSentBadge).toBe('Filled · TS response sent');

    const selectedBadge = gptDiagnosticsBadgeTextForTest({
      requestNumber: 1,
      isEmpty: false,
      incompleteSequence: false,
      durations: {},
      delivery: 'trusted_server_selected',
    });
    expect(selectedBadge).toBe('Filled · TS selected');

    const pendingBadge = gptDiagnosticsBadgeTextForTest({
      requestNumber: 1,
      isEmpty: false,
      incompleteSequence: false,
      durations: {},
      requestPath: 'trusted_server_direct',
      trustedServerOpportunity: 'renderable_candidate',
      delivery: 'pending',
    });
    expect(pendingBadge).toBe('Filled · TS candidate (pending)');

    const unconfirmedBadge = gptDiagnosticsBadgeTextForTest({
      requestNumber: 1,
      isEmpty: false,
      incompleteSequence: false,
      durations: {},
      requestPath: 'trusted_server_direct',
      trustedServerOpportunity: 'renderable_candidate',
      delivery: 'candidate_unconfirmed',
    });
    expect(unconfirmedBadge).toBe('Filled · TS unconfirmed');

    const noCandidateBadge = gptDiagnosticsBadgeTextForTest({
      requestNumber: 1,
      isEmpty: false,
      incompleteSequence: false,
      durations: {},
      requestPath: 'trusted_server_direct',
      trustedServerOpportunity: 'no_candidate',
      delivery: 'no_candidate',
    });
    expect(noCandidateBadge).toBe('Filled · No TS candidate');

    const unknownBadge = gptDiagnosticsBadgeTextForTest({
      requestNumber: 1,
      isEmpty: false,
      incompleteSequence: false,
      durations: {},
      requestPath: 'prebid_refresh',
      delivery: 'unknown',
      adManager: { lineItemId: 6543210987 },
    });
    expect(unknownBadge).toBe('Filled · Delivery unknown');
    expect(unknownBadge).not.toMatch(/other demand|other GAM|6543210987/i);

    const notApplicableBadge = gptDiagnosticsBadgeTextForTest({
      requestNumber: 1,
      incompleteSequence: false,
      durations: {},
      requestPath: 'unattributed',
      delivery: 'not_applicable',
    });
    expect(notApplicableBadge).toBe('Pending');

    for (const badge of [
      responseSentBadge,
      selectedBadge,
      pendingBadge,
      unconfirmedBadge,
      noCandidateBadge,
      unknownBadge,
      notApplicableBadge,
    ]) {
      expect(badge).not.toMatch(
        /creative rendered|other demand won|no Trusted Server creative ran|ad visible|pixels confirmed/i
      );
    }
  });

  it('uses only GPT-observed lifecycle facts in badge text', () => {
    expect(
      formatGptDiagnosticsBadgeText({
        requestNumber: 1,
        requestedAtMs: 0,
        responseAtMs: 276,
        renderAtMs: 318,
        viewableAtMs: 1318,
        isEmpty: false,
        requestedSlotSizes: [
          [728, 90],
          [970, 250],
        ],
        size: [728, 90],
        observedSlotSize: [980, 270],
        incompleteSequence: false,
        durations: {
          requestToResponseMs: 276,
          responseToRenderMs: 42,
          renderToViewableMs: 1000,
        },
      })
    ).toBe(
      'Filled · Req 728×90, 970×250 · Fill 728×90 · Box 980×270\nResponse 276 ms · Render 42 ms\nViewable after 1 s'
    );
    expect(
      formatGptDiagnosticsBadgeText({
        requestNumber: 1,
        isEmpty: false,
        requestedSlotSizes: [
          [300, 250],
          [320, 50],
          [728, 90],
          [970, 250],
        ],
        incompleteSequence: false,
        durations: {},
      })
    ).toBe('Filled · Req 300×250, 320×50, 728×90 +1');
    expect(
      gptDiagnosticsBadgeTextForTest({
        requestNumber: 1,
        isEmpty: true,
        incompleteSequence: false,
        durations: {},
      })
    ).toBe('Empty');
    expect(
      formatGptDiagnosticsBadgeText({
        requestNumber: 1,
        renderAtMs: 5,
        incompleteSequence: false,
        durations: {},
      })
    ).toBe('Rendered (fill unknown)');
    expect(
      formatGptDiagnosticsBadgeText({
        requestNumber: 1,
        incompleteSequence: false,
        durations: {},
      })
    ).toBe('Pending');
    expect(formatGptDiagnosticsBadgeText.toString()).not.toMatch(
      /Trusted Server|GAM winner|Prebid|bidder|provenance/i
    );
  });

  it('rejects a counterfeit bound element instead of accepting DOM-shaped data', () => {
    const frames: Array<() => void> = [];
    const store = new GptDiagnosticsStore({ schedule: (callback) => callback() });
    store.recordSlotRequested(slot('counterfeit'));
    const bindings = new FakeBindings();
    const counterfeit = Object.freeze({
      getBoundingClientRect: () => rectangle(10, 10, 300, 250),
      isConnected: true,
    }) as unknown as HTMLElement;
    bindings.set(1, { status: 'bound' }, counterfeit, true);
    const layer = document.createElement('div');
    document.body.append(layer);
    const manager = new GptDiagnosticsBadgeManager(store, bindings, {
      scheduleFrame: queueFrame(frames),
    });

    manager.setLayer(layer);
    runFrame(frames);

    expect(layer.querySelector('.tsgd-badge')).toBeNull();
    manager.destroy();
  });

  it('positions in the overlay layer and coalesces scroll and resize updates', () => {
    const frames: Array<() => void> = [];
    let currentRectangle = rectangle(100, 120, 300, 250);
    const element = document.createElement('div');
    element.id = 'publisher-slot';
    element.className = 'publisher-class';
    element.style.width = '300px';
    document.body.append(element);
    vi.spyOn(element, 'getBoundingClientRect').mockImplementation(() => currentRectangle);
    const originalAttributes = element
      .getAttributeNames()
      .map((name) => [name, element.getAttribute(name)]);

    const store = new GptDiagnosticsStore({ schedule: (callback) => callback() });
    store.recordSlotRequested(slot('publisher-slot'));
    const bindings = new FakeBindings();
    bindings.set(1, { status: 'bound' }, element, true);
    const layer = document.createElement('div');
    document.body.append(layer);
    const manager = new GptDiagnosticsBadgeManager(store, bindings, {
      scheduleFrame: queueFrame(frames),
    });
    manager.setLayer(layer);
    runFrame(frames);

    const firstBadge = layer.querySelector<HTMLElement>('.tsgd-badge')!;
    expect(firstBadge.style.left).toBe('100px');
    expect(firstBadge.style.top).toBe('112px');
    expect(firstBadge.style.maxWidth).toBe('260px');

    currentRectangle = rectangle(220, 260, 300, 250);
    window.dispatchEvent(new Event('scroll'));
    window.dispatchEvent(new Event('resize'));
    expect(frames).toHaveLength(1);
    runFrame(frames);
    const movedBadge = layer.querySelector<HTMLElement>('.tsgd-badge')!;
    expect(movedBadge.style.left).toBe('220px');
    expect(movedBadge.style.top).toBe('252px');
    expect(element.getAttributeNames().map((name) => [name, element.getAttribute(name)])).toEqual(
      originalAttributes
    );
    manager.destroy();
  });

  it('ignores unrelated and hidden-layer mutations but tracks known slot subtrees', async () => {
    const frames: Array<() => void> = [];
    const store = new GptDiagnosticsStore({ schedule: (callback) => callback() });
    const bindings = new FakeBindings();
    const element = document.createElement('div');
    element.id = 'mutation-slot';
    document.body.append(element);
    vi.spyOn(element, 'getBoundingClientRect').mockReturnValue(rectangle(10, 100, 300, 250));
    store.recordSlotRequested(slot('mutation-slot'));
    bindings.set(1, { status: 'bound' }, element, true);
    const layer = document.createElement('div');
    document.body.append(layer);
    const manager = new GptDiagnosticsBadgeManager(store, bindings, {
      scheduleFrame: queueFrame(frames),
    });
    manager.setLayer(layer);
    runFrame(frames);
    const snapshot = vi.spyOn(store, 'snapshot');
    snapshot.mockClear();

    document.body.append(document.createElement('div'));
    await Promise.resolve();
    expect(frames).toEqual([]);
    expect(snapshot).not.toHaveBeenCalled();

    element.append(document.createElement('span'));
    await Promise.resolve();
    expect(frames).toHaveLength(1);
    runFrame(frames);
    expect(snapshot).not.toHaveBeenCalled();

    manager.setLayer(undefined);
    runFrame(frames);
    snapshot.mockClear();
    element.append(document.createElement('span'));
    await Promise.resolve();
    expect(frames).toEqual([]);
    expect(snapshot).not.toHaveBeenCalled();
    manager.destroy();
  });

  it('degrades when optional observer APIs are unavailable', () => {
    const frames: Array<() => void> = [];
    const layer = document.createElement('div');
    document.body.append(layer);
    const store = new GptDiagnosticsStore();
    const bindings = new FakeBindings();
    const manager = new GptDiagnosticsBadgeManager(store, bindings, {
      window: Object.assign(window, {
        MutationObserver: undefined,
        ResizeObserver: undefined,
      }),
      scheduleFrame: queueFrame(frames),
    });

    expect(() => {
      manager.setLayer(layer);
      runFrame(frames);
    }).not.toThrow();
    manager.destroy();
  });

  it('cancels a pending badge update on destroy and suppresses a hostile late callback', () => {
    const frames: Array<() => void> = [];
    const cancel = vi.fn();
    const layer = document.createElement('div');
    document.body.append(layer);
    const manager = new GptDiagnosticsBadgeManager(new GptDiagnosticsStore(), new FakeBindings(), {
      scheduleFrame: (callback) => {
        frames.push(callback);
        return cancel;
      },
    });
    const update = vi.spyOn(manager, 'update');
    manager.setLayer(layer);

    manager.destroy();
    frames[0]?.();

    expect(cancel).toHaveBeenCalledOnce();
    expect(update).not.toHaveBeenCalled();
  });

  it('runs one scheduled badge callback at most once', () => {
    const frames: Array<() => void> = [];
    const manager = new GptDiagnosticsBadgeManager(new GptDiagnosticsStore(), new FakeBindings(), {
      scheduleFrame: (callback) => {
        frames.push(callback);
        return vi.fn();
      },
    });
    const update = vi.spyOn(manager, 'update');
    manager.setLayer(document.createElement('div'));

    frames[0]?.();
    frames[0]?.();

    expect(update).toHaveBeenCalledOnce();
    manager.destroy();
  });

  it('isolates a hostile frame cancellation during destroy', () => {
    const cancel = vi.fn(() => {
      throw new Error('cancel failed');
    });
    const manager = new GptDiagnosticsBadgeManager(new GptDiagnosticsStore(), new FakeBindings(), {
      scheduleFrame: () => cancel,
    });
    manager.setLayer(document.createElement('div'));

    expect(() => manager.destroy()).not.toThrow();
    expect(cancel).toHaveBeenCalledOnce();
  });
});
