import { afterEach, describe, expect, it, vi } from 'vitest';
import { installAdTraceOverlay } from '../../../src/integrations/ad_trace/overlay';
import type { AdTraceApi } from '../../../src/core/types';

function api(): AdTraceApi {
  const slot = {
    slotId: 'slot-a',
    latestGeneration: 1,
    generations: [],
    stages: {
      trustedServer: { outcome: 'won', confidence: 'definitive', reason: 'winner' },
      prebid: { outcome: 'not_run', confidence: 'definitive', reason: 'direct' },
      gam: { outcome: 'trusted_server_candidate', confidence: 'probable', reason: 'render' },
      creative: { outcome: 'not_observed', confidence: 'none', reason: 'none' },
    },
  } as const;
  const renders = [
    {
      sequence: 1,
      slotId: 'slot-a',
      generation: 1,
      source: 'gpt',
      outcome: 'gam_only',
      confidence: 'probable',
      visibility: 'unknown',
      createdAt: 1,
      updatedAt: 1,
    },
  ] as const;
  return {
    getSlot: () => slot as any,
    getEvents: () => [],
    getRenderTimeline: () => renders as any,
    export: () => ({
      version: 1,
      slots: [slot as any],
      events: [],
      renders: renders as any,
      metadata: { droppedEvents: 0, evictedSlots: 0 },
    }),
  };
}

describe('ad trace overlay lifecycle', () => {
  afterEach(() => {
    document.getElementById('ts-ad-trace-overlay')?.remove();
    document.getElementById('slot-prefix-rendered')?.remove();
    delete window.tsjs;
    vi.restoreAllMocks();
  });

  it('finds prefix slots, observes resize, and coalesces animation frames', () => {
    const element = document.createElement('div');
    element.id = 'slot-prefix-rendered';
    const rect = vi.spyOn(element, 'getBoundingClientRect').mockReturnValue({
      left: 10,
      top: 20,
      width: 300,
      height: 250,
    } as DOMRect);
    document.body.appendChild(element);
    const updateVisibility = vi.fn();
    window.tsjs = {
      adSlots: [{ id: 'slot-a', div_id: 'slot-prefix' }],
      getAdTraceElement: () => element,
      updateAdTraceVisibility: updateVisibility,
    } as any;

    const observe = vi.fn();
    const attachShadow = HTMLElement.prototype.attachShadow;
    let shadow: ShadowRoot | undefined;
    vi.spyOn(HTMLElement.prototype, 'attachShadow').mockImplementation(function (
      this: HTMLElement,
      init: ShadowRootInit
    ) {
      shadow = attachShadow.call(this, init);
      return shadow;
    });
    vi.stubGlobal(
      'ResizeObserver',
      class {
        observe = observe;
        unobserve = vi.fn();
        disconnect = vi.fn();
      }
    );
    const frames: FrameRequestCallback[] = [];
    vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => {
      frames.push(callback);
      return frames.length;
    });
    let subscriber: (() => void) | undefined;
    installAdTraceOverlay(api(), (listener) => {
      subscriber = listener;
      return vi.fn();
    });

    expect(rect).toHaveBeenCalledTimes(1);
    expect(observe).toHaveBeenCalledWith(element);
    expect(updateVisibility).toHaveBeenCalledWith('slot-a', 1, 'visible');
    const badge = shadow?.querySelector('.badge');
    const row = shadow?.querySelector('.row');
    expect(badge?.textContent).toBe(
      'Trusted Server selected a bid\nGAM rendered an ad — source not attributed\nSlot element currently visible'
    );
    expect(row?.textContent).toContain('GAM rendered an ad — source not attributed');
    expect(`${badge?.textContent}\n${row?.textContent}`).not.toMatch(
      /definitive|strong|probable|not_run|gam_only|TS winner|Prebid winner|#1/
    );
    expect(element.getAttribute('data-ts-trace-seq')).toBe('1');
    expect(element.getAttribute('data-ts-trace-outcome')).toBe('gam_only');
    window.dispatchEvent(new Event('scroll'));
    window.dispatchEvent(new Event('scroll'));
    subscriber?.();
    expect(frames).toHaveLength(1);
    frames.shift()?.(1);
    expect(rect).toHaveBeenCalledTimes(2);

    const replacement = document.createElement('div');
    replacement.id = element.id;
    element.replaceWith(replacement);
    subscriber?.();
    frames.shift()?.(2);
    expect(replacement.hasAttribute('data-ts-trace-seq')).toBe(false);
    expect(element.hasAttribute('data-ts-trace-seq')).toBe(false);
    expect(updateVisibility).toHaveBeenCalledWith('slot-a', 1, 'disconnected');
  });

  it('does not add an empty badge when no operator-facing fact was observed', () => {
    const element = document.createElement('div');
    document.body.appendChild(element);
    window.tsjs = {
      getAdTraceElement: () => element,
      updateAdTraceVisibility: vi.fn(),
    } as any;
    const attachShadow = HTMLElement.prototype.attachShadow;
    let shadow: ShadowRoot | undefined;
    vi.spyOn(HTMLElement.prototype, 'attachShadow').mockImplementation(function (
      this: HTMLElement,
      init: ShadowRootInit
    ) {
      shadow = attachShadow.call(this, init);
      return shadow;
    });
    vi.stubGlobal(
      'ResizeObserver',
      class {
        observe() {}
        unobserve() {}
        disconnect() {}
      }
    );
    vi.spyOn(element, 'getBoundingClientRect').mockReturnValue({
      left: 0,
      top: 0,
      width: 300,
      height: 250,
    } as DOMRect);
    const slot = {
      slotId: 'slot-a',
      latestGeneration: 1,
      generations: [],
      stages: {
        trustedServer: { outcome: 'not_observed', confidence: 'none', reason: 'none' },
        prebid: { outcome: 'not_observed', confidence: 'none', reason: 'none' },
        gam: { outcome: 'not_observed', confidence: 'none', reason: 'none' },
        creative: { outcome: 'not_observed', confidence: 'none', reason: 'none' },
      },
    };
    const render = {
      sequence: 1,
      slotId: 'slot-a',
      generation: 1,
      source: 'gpt',
      outcome: 'unresolved',
      confidence: 'none',
      visibility: 'unknown',
      createdAt: 1,
      updatedAt: 1,
    };
    installAdTraceOverlay(
      {
        getSlot: () => slot as any,
        getEvents: () => [],
        getRenderTimeline: () => [render] as any,
        export: () =>
          ({
            version: 1,
            slots: [slot],
            events: [],
            renders: [render],
            metadata: { droppedEvents: 0, evictedSlots: 0 },
          }) as any,
      },
      () => vi.fn()
    );

    expect(shadow?.querySelector('.badge')).toBeNull();
    expect(shadow?.querySelector('.row')?.textContent).toContain('No trace result observed');
  });

  it('uses observed stage evidence when a render row has no render outcome', () => {
    const attachShadow = HTMLElement.prototype.attachShadow;
    let shadow: ShadowRoot | undefined;
    vi.spyOn(HTMLElement.prototype, 'attachShadow').mockImplementation(function (
      this: HTMLElement,
      init: ShadowRootInit
    ) {
      shadow = attachShadow.call(this, init);
      return shadow;
    });
    vi.stubGlobal(
      'ResizeObserver',
      class {
        observe() {}
        unobserve() {}
        disconnect() {}
      }
    );
    const stages = {
      trustedServer: { outcome: 'won', confidence: 'definitive', reason: 'winner' },
      prebid: { outcome: 'won', confidence: 'definitive', reason: 'selected_targeting' },
      gam: { outcome: 'not_observed', confidence: 'none', reason: 'none' },
      creative: { outcome: 'render_failed', confidence: 'definitive', reason: 'failure' },
    };
    const slot = {
      slotId: 'slot-a',
      latestGeneration: 1,
      generations: [{ generation: 1, stages }],
      stages,
    };
    const render = {
      sequence: 1,
      slotId: 'slot-a',
      generation: 1,
      source: 'gpt',
      outcome: 'unresolved',
      confidence: 'none',
      visibility: 'unknown',
      createdAt: 1,
      updatedAt: 1,
    };
    installAdTraceOverlay(
      {
        getSlot: () => slot as any,
        getEvents: () => [],
        getRenderTimeline: () => [render] as any,
        export: () =>
          ({
            version: 1,
            slots: [slot],
            events: [],
            renders: [render],
            metadata: { droppedEvents: 0, evictedSlots: 0 },
          }) as any,
      },
      () => vi.fn()
    );

    const row = shadow?.querySelector('.row');
    expect(row?.textContent).toContain('Prebid reported render failed');
    expect(row?.textContent).not.toContain('No trace result observed');
  });

  it('gives a retained render a factual status after its generation stages were evicted', () => {
    const attachShadow = HTMLElement.prototype.attachShadow;
    let shadow: ShadowRoot | undefined;
    vi.spyOn(HTMLElement.prototype, 'attachShadow').mockImplementation(function (
      this: HTMLElement,
      init: ShadowRootInit
    ) {
      shadow = attachShadow.call(this, init);
      return shadow;
    });
    vi.stubGlobal(
      'ResizeObserver',
      class {
        observe() {}
        unobserve() {}
        disconnect() {}
      }
    );
    const slot = {
      slotId: 'slot-a',
      latestGeneration: 2,
      generations: [],
      stages: {
        trustedServer: { outcome: 'not_observed', confidence: 'none', reason: 'none' },
        prebid: { outcome: 'not_observed', confidence: 'none', reason: 'none' },
        gam: { outcome: 'not_observed', confidence: 'none', reason: 'none' },
        creative: { outcome: 'not_observed', confidence: 'none', reason: 'none' },
      },
    };
    const render = {
      sequence: 1,
      slotId: 'slot-a',
      generation: 1,
      source: 'gpt',
      outcome: 'gam_only',
      confidence: 'probable',
      visibility: 'unknown',
      createdAt: 1,
      updatedAt: 1,
    };
    installAdTraceOverlay(
      {
        getSlot: () => slot as any,
        getEvents: () => [],
        getRenderTimeline: () => [render] as any,
        export: () =>
          ({
            version: 1,
            slots: [slot],
            events: [],
            renders: [render],
            metadata: { droppedEvents: 0, evictedSlots: 0 },
          }) as any,
      },
      () => vi.fn()
    );

    const row = shadow?.querySelector('.row');
    expect(row?.textContent).toContain('GAM rendered an ad — source not attributed');
    expect(row?.textContent).not.toMatch(/probable|gam_only|#1/);
  });
});
