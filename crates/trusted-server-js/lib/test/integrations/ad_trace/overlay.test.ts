import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  installAdTraceOverlay,
  placeTraceBadges,
} from '../../../src/integrations/ad_trace/overlay';
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
    export: () =>
      ({
        version: 1,
        slots: [slot as any],
        events: [],
        renders: renders as any,
        metadata: { droppedEvents: 0, evictedSlots: 0 },
      }) as any,
  };
}

describe('trace badge layout', () => {
  it('stacks collisions deterministically and omits viewport overflow', () => {
    expect(
      placeTraceBadges(
        [
          { key: 'slot-b', left: 10, top: 10, width: 40, height: 20 },
          { key: 'slot-a', left: 10, top: 10, width: 40, height: 20 },
          { key: 'slot-c', left: 10, top: 10, width: 40, height: 20 },
        ],
        100,
        60
      )
    ).toEqual([
      { key: 'slot-a', left: 10, top: 10 },
      { key: 'slot-b', left: 10, top: 34 },
    ]);
  });
});

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
    expect(badge?.querySelector('.badge-status')?.textContent).toBe('GAM ad');
    expect(badge?.querySelector('.badge-container')?.textContent).toBe('slot-prefix-rendered');
    expect(badge?.getAttribute('aria-label')).toBe('GAM ad; container slot-prefix-rendered');
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

  it('keeps offscreen anchors in the panel without piling badges at the viewport edge', () => {
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
      left: 20,
      top: -300,
      width: 300,
      height: 250,
    } as DOMRect);

    installAdTraceOverlay(api(), () => vi.fn());

    expect(shadow?.querySelector('.badge')).toBeNull();
    expect(shadow?.querySelector('.row')?.textContent).toContain(
      'GAM rendered an ad — source not attributed'
    );
    element.remove();
  });

  it('shows response and visibility callback coverage as correlated totals', () => {
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
    const coverage = Object.fromEntries(
      [
        'gpt_requests',
        'gpt_responses',
        'gpt_renders',
        'gpt_loads',
        'gpt_visibility',
        'gpt_viewability',
        'prebid_render_succeeded',
        'prebid_render_failed',
      ].map((category) => [
        category,
        { observed: 0, correlated: 0, ambiguous: 0, unmatched: 0, ignored: 0 },
      ])
    ) as Record<string, Record<string, number>>;
    coverage.gpt_responses = {
      observed: 2,
      correlated: 1,
      ambiguous: 0,
      unmatched: 1,
      ignored: 0,
    };
    coverage.gpt_visibility = {
      observed: 3,
      correlated: 2,
      ambiguous: 1,
      unmatched: 0,
      ignored: 0,
    };
    const traceApi = api();
    const exported = traceApi.export();
    installAdTraceOverlay(
      {
        ...traceApi,
        export: () => ({
          ...exported,
          metadata: { ...exported.metadata, coverage, anomalies: {} },
        }),
      } as AdTraceApi,
      () => vi.fn()
    );

    expect(shadow?.querySelector('.health')?.textContent).toContain(
      'GPT responses: 1/2 correlated'
    );
    expect(shadow?.querySelector('.health')?.textContent).toContain(
      'GPT visibility: 2/3 correlated'
    );
  });

  it('filters panel rows and badges without changing trace ownership', () => {
    const element = document.createElement('div');
    document.body.appendChild(element);
    const updateVisibility = vi.fn();
    window.tsjs = {
      getAdTraceElement: () => element,
      updateAdTraceVisibility: updateVisibility,
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
    vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => {
      callback(0);
      return 1;
    });
    vi.spyOn(element, 'getBoundingClientRect').mockReturnValue({
      left: 10,
      top: 20,
      width: 300,
      height: 250,
    } as DOMRect);

    installAdTraceOverlay(api(), () => vi.fn());
    expect(shadow?.querySelector('.row')).not.toBeNull();
    expect(shadow?.querySelector('.badge')).not.toBeNull();
    const filter = shadow?.querySelector('select');
    if (filter) {
      filter.value = 'empty';
      filter.dispatchEvent(new Event('change'));
    }

    expect(shadow?.querySelector('.row')).toBeNull();
    expect(shadow?.querySelector('.badge')).toBeNull();
    expect(updateVisibility).toHaveBeenCalled();
    element.remove();
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

  it('gives retained render history a factual status after its slot was evicted', () => {
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
        getSlot: () => undefined,
        getEvents: () => [],
        getRenderTimeline: () => [render] as any,
        export: () =>
          ({
            version: 1,
            slots: [],
            events: [],
            renders: [render],
            metadata: { droppedEvents: 0, evictedSlots: 1 },
          }) as any,
      },
      () => vi.fn()
    );

    const row = shadow?.querySelector('.row');
    expect(row?.textContent).toContain('GAM rendered an ad — source not attributed');
    expect(row?.textContent).not.toMatch(/probable|gam_only|#1/);
  });
});
