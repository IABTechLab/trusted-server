import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  GptDiagnosticsDataApiController,
  type GptDiagnosticsPresentationFactory,
} from '../../../src/integrations/gpt_diagnostics/data_api';
import { GPT_DIAGNOSTICS_HOST_ID } from '../../../src/integrations/gpt_diagnostics/overlay';
import {
  createDiagnosticsPresentationIntegrationRegistration,
  createRenderTracePresentation,
} from '../../../src/integrations/gpt_diagnostics/presentation';
import { GptDiagnosticsStore } from '../../../src/integrations/gpt_diagnostics/store';
import { createRenderTraceStore } from '../../../src/core/trace';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  PreparedIntegration,
} from '../../../src/kernel/integration_registry';

const RELEASE_ID = 'a'.repeat(64);
let frames: Array<() => void> = [];

beforeEach(() => {
  document.body.replaceChildren();
  frames = [];
  vi.spyOn(document, 'readyState', 'get').mockReturnValue('complete');
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
    const frame = () => callback(0);
    frames.push(frame);
    return frames.length;
  });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  document.body.replaceChildren();
});

function drainFrames(): void {
  let count = 0;
  while (frames.length > 0 && count < 16) {
    frames.shift()?.();
    count += 1;
  }
  if (frames.length > 0) throw new Error('Diagnostics presentation did not quiesce');
}

function presentationInterfaces(runtimeDocument: unknown) {
  return Object.freeze({
    'runtime.v1': Object.freeze({
      boot: () =>
        Object.freeze({
          diagnostics: Object.freeze({
            version: 1,
            renderTraceOverlay: true,
            gpt: Object.freeze({ active: false }),
          }),
        }),
      document: runtimeDocument,
    }),
    'trace.presentation.v1': Object.freeze({
      attachPresentation: vi.fn(() => vi.fn()),
    }),
  });
}

function preparePresentation(runtimeDocument: unknown): PreparedIntegration {
  return createDiagnosticsPresentationIntegrationRegistration(RELEASE_ID).prepare(
    Object.freeze({
      config: undefined,
      interfaces: presentationInterfaces(runtimeDocument),
      signal: new AbortController().signal,
      onDispose: vi.fn(),
    } satisfies IntegrationPrepareContext)
  ) as PreparedIntegration;
}

describe('deferred GPT diagnostics presentation integration', () => {
  it('binds a foreign-realm slot mutation and renders its badge through the registration', async () => {
    const frame = document.createElement('iframe');
    document.body.append(frame);
    const foreignDocument = frame.contentDocument;
    const foreignWindow = frame.contentWindow;
    if (!foreignDocument || !foreignWindow) throw new Error('Expected an iframe document realm');
    const foreignRealm = foreignWindow as Window & typeof globalThis;
    vi.spyOn(foreignDocument, 'readyState', 'get').mockReturnValue('complete');
    Object.defineProperty(foreignWindow, 'CSS', {
      configurable: true,
      value: Object.freeze({
        escape: (value: string) => value.replace(/[^a-zA-Z0-9_-]/g, '\\$&'),
      }),
    });
    const replaceChildren = vi.spyOn(foreignRealm.Element.prototype, 'replaceChildren');
    const store = new GptDiagnosticsStore({ schedule: (callback) => callback() });
    const slotToken = Object.freeze({
      getAdUnitPath: () => '/foreign/slot',
      getSlotElementId: () => 'foreign-mutation-slot',
    });
    store.recordSlotRequested(slotToken, 1);
    const controller = new GptDiagnosticsDataApiController(store, {
      location: foreignWindow.location,
      schedule: (callback) => {
        callback();
        return () => undefined;
      },
    });
    const releases: Array<() => void> = [];
    const prepared = createDiagnosticsPresentationIntegrationRegistration(RELEASE_ID).prepare(
      Object.freeze({
        config: undefined,
        interfaces: Object.freeze({
          'runtime.v1': Object.freeze({
            boot: () =>
              Object.freeze({
                diagnostics: Object.freeze({
                  version: 1,
                  renderTraceOverlay: false,
                  gpt: Object.freeze({ active: true }),
                }),
              }),
            document: foreignDocument,
          }),
          'trace.presentation.v1': Object.freeze({
            attachPresentation: vi.fn(() => vi.fn()),
          }),
          'gpt_diag.v1': Object.freeze({
            api: controller.api,
            attachPresentation: (factory: GptDiagnosticsPresentationFactory) =>
              controller.attachPresentation(factory),
          }),
        }),
        signal: new AbortController().signal,
        onDispose: vi.fn(),
      } satisfies IntegrationPrepareContext)
    ) as PreparedIntegration;

    prepared.activate(
      Object.freeze({
        signal: new AbortController().signal,
        onDispose: (release: () => void) => releases.push(release),
        afterCommit: vi.fn(),
      } satisfies IntegrationActivationContext)
    );
    drainFrames();
    expect(controller.api.snapshot().slots[0]?.binding).toEqual({
      status: 'unbound',
      reason: 'missing_element',
    });

    const foreignSlot = foreignDocument.createElement('div');
    foreignSlot.id = 'foreign-mutation-slot';
    vi.spyOn(foreignSlot, 'getBoundingClientRect').mockReturnValue({
      bottom: 260,
      height: 250,
      left: 10,
      right: 310,
      top: 10,
      width: 300,
      x: 10,
      y: 10,
      toJSON: () => ({}),
    } as DOMRect);
    expect(foreignSlot).toBeInstanceOf(foreignRealm.Element);
    expect(foreignSlot).not.toBeInstanceOf(window.Element);
    foreignDocument.body.append(foreignSlot);

    await vi.waitFor(() => {
      drainFrames();
      expect(controller.api.snapshot().slots[0]?.binding).toEqual({ status: 'bound' });
    });
    store.recordSlotRenderEnded(slotToken, { isEmpty: false, size: [300, 250] }, 2);
    await vi.waitFor(() => {
      drainFrames();
      expect(controller.api.snapshot().slots[0]?.requests[0]?.observedSlotSize).toEqual([300, 250]);
    });
    const badgeRenderCount = (): number =>
      replaceChildren.mock.calls.filter((nodes) =>
        nodes.some(
          (node) => node instanceof foreignRealm.Element && node.classList.contains('tsgd-badge')
        )
      ).length;
    expect(badgeRenderCount()).toBeGreaterThan(0);

    const renderedBeforeDispose = badgeRenderCount();
    releases.reverse().forEach((release) => release());
    expect(replaceChildren.mock.calls[replaceChildren.mock.calls.length - 1]).toEqual([]);
    foreignSlot.remove();
    await Promise.resolve();
    drainFrames();
    expect(badgeRenderCount()).toBe(renderedBeforeDispose);
    controller.destroy();
    frame.remove();
  });

  it('accepts a valid foreign-realm Document at the registration boundary', () => {
    const frame = document.createElement('iframe');
    document.body.append(frame);
    const foreignDocument = frame.contentDocument;
    const foreignWindow = frame.contentWindow;
    if (!foreignDocument || !foreignWindow) throw new Error('Expected an iframe document realm');
    const foreignRealm = foreignWindow as Window & typeof globalThis;
    expect(foreignDocument).not.toBeInstanceOf(window.Document);
    expect(foreignDocument).toBeInstanceOf(foreignRealm.Document);

    expect(() => preparePresentation(foreignDocument)).not.toThrow();

    frame.remove();
  });

  it.each([
    ['plain record', Object.freeze({})],
    [
      'counterfeit realm',
      Object.freeze({ defaultView: Object.freeze({ Document: class CounterfeitDocument {} }) }),
    ],
    [
      'hostile defaultView',
      Object.freeze(
        Object.defineProperty({}, 'defaultView', {
          get: () => {
            throw new Error('hostile defaultView');
          },
        })
      ),
    ],
  ])('rejects a %s runtime Document candidate at the registration boundary', (_name, candidate) => {
    expect(() => preparePresentation(candidate)).toThrow(
      'diagnostics presentation capability graph is malformed'
    );
  });

  it.each(['render trace', 'GPT'] as const)(
    'throws for an invalid %s presentation disposer so the deferred transaction rolls back',
    (failedSurface) => {
      const traceRelease = vi.fn();
      const gptRelease = vi.fn();
      const attachTrace = vi.fn(() =>
        failedSurface === 'render trace' ? (undefined as never) : traceRelease
      );
      const attachGpt = vi.fn(() => (failedSurface === 'GPT' ? (undefined as never) : gptRelease));
      const releases: Array<() => void> = [];
      const runtime = Object.freeze({
        boot: () =>
          Object.freeze({
            diagnostics: Object.freeze({
              version: 1,
              renderTraceOverlay: true,
              gpt: Object.freeze({ active: true }),
            }),
          }),
        document,
      });
      const prepared = createDiagnosticsPresentationIntegrationRegistration(RELEASE_ID).prepare(
        Object.freeze({
          config: undefined,
          interfaces: Object.freeze({
            'runtime.v1': runtime,
            'trace.presentation.v1': Object.freeze({
              attachPresentation: attachTrace,
            }),
            'gpt_diag.v1': Object.freeze({
              api: Object.freeze({}),
              attachPresentation: attachGpt,
            }),
          }),
          signal: new AbortController().signal,
          onDispose: vi.fn(),
        } satisfies IntegrationPrepareContext)
      ) as PreparedIntegration;

      expect(() =>
        prepared.activate(
          Object.freeze({
            signal: new AbortController().signal,
            onDispose: (callback: () => void) => releases.push(callback),
            afterCommit: vi.fn(),
          } satisfies IntegrationActivationContext)
        )
      ).toThrow(
        failedSurface === 'render trace'
          ? 'render trace presentation disposer is unavailable'
          : 'GPT diagnostics presentation disposer is unavailable'
      );
      expect(attachTrace).toHaveBeenCalledOnce();
      expect(attachGpt).toHaveBeenCalledTimes(failedSurface === 'render trace' ? 0 : 1);
      releases.reverse().forEach((release) => release());
      expect(traceRelease).toHaveBeenCalledTimes(failedSurface === 'GPT' ? 1 : 0);
      expect(gptRelease).not.toHaveBeenCalled();
    }
  );

  it('uses the target document realm when stamping a render slot', () => {
    const frame = document.createElement('iframe');
    document.body.append(frame);
    const targetDocument = frame.contentDocument;
    const targetWindow = frame.contentWindow;
    if (!targetDocument || !targetWindow) throw new Error('Expected an iframe document realm');
    const targetRealm = targetWindow as Window & typeof globalThis;
    const slot = targetDocument.createElement('div');
    slot.id = 'foreign-realm-slot';
    targetDocument.body.append(slot);
    expect(slot).toBeInstanceOf(targetRealm.HTMLElement);
    expect(slot).not.toBeInstanceOf(window.HTMLElement);
    const renderTrace = createRenderTraceStore();
    renderTrace.record({
      slotId: slot.id,
      elementId: slot.id,
      path: 'auction',
      rendered: true,
      injected: true,
      visible: true,
    });

    const detach = renderTrace.attachPresentation((source) =>
      createRenderTracePresentation(source, { document: targetDocument })
    );

    expect(slot.getAttribute('data-ts-rendered')).toBe('true');
    expect(slot.querySelector('.ts-render-badge')).not.toBeNull();
    detach();
    expect(slot.getAttribute('data-ts-rendered')).toBeNull();
    renderTrace.dispose();
    frame.remove();
  });

  it('replays and owns render-trace presentation without GPT diagnostics', () => {
    const traceTasks: Array<() => void> = [];
    const renderTrace = createRenderTraceStore({
      schedule: (callback) => {
        traceTasks.push(callback);
        return () => {
          const index = traceTasks.indexOf(callback);
          if (index >= 0) traceTasks.splice(index, 1);
        };
      },
    });
    const diagnostics = renderTrace.diagnostics;
    const slot = document.createElement('div');
    slot.id = 'render-overlay-only-slot';
    document.body.append(slot);
    renderTrace.record({
      slotId: slot.id,
      elementId: slot.id,
      path: 'ssat',
      rendered: true,
      injected: true,
      visible: true,
    });
    const activationRelease: Array<() => void> = [];
    const runtime = Object.freeze({
      boot: () =>
        Object.freeze({
          diagnostics: Object.freeze({
            version: 1,
            renderTraceOverlay: true,
            gpt: Object.freeze({ active: false }),
          }),
        }),
      document,
    });
    const trace = Object.freeze({
      attachPresentation: renderTrace.attachPresentation,
    });
    const prepared = createDiagnosticsPresentationIntegrationRegistration(RELEASE_ID).prepare(
      Object.freeze({
        config: undefined,
        interfaces: Object.freeze({
          'runtime.v1': runtime,
          'trace.presentation.v1': trace,
        }),
        signal: new AbortController().signal,
        onDispose: vi.fn(),
      } satisfies IntegrationPrepareContext)
    ) as PreparedIntegration;

    expect(traceTasks).toEqual([]);
    expect(slot.getAttributeNames().filter((name) => name.startsWith('data-ts-'))).toEqual([]);
    expect(document.getElementById('ts-render-trace-panel')).toBeNull();

    prepared.activate(
      Object.freeze({
        signal: new AbortController().signal,
        onDispose: (callback: () => void) => activationRelease.push(callback),
        afterCommit: vi.fn(),
      } satisfies IntegrationActivationContext)
    );

    expect(traceTasks).toEqual([]);
    expect(slot.getAttribute('data-ts-rendered')).toBe('true');
    expect(slot.querySelector('.ts-render-badge')).not.toBeNull();
    expect(document.getElementById('ts-render-trace-panel')?.textContent).toContain(slot.id);
    expect(renderTrace.diagnostics).toBe(diagnostics);

    renderTrace.enrich(1, { bidder: 'later-bidder' });
    expect(traceTasks).toHaveLength(1);
    activationRelease.reverse().forEach((release) => release());
    expect(traceTasks).toEqual([]);
    expect(slot.getAttributeNames().filter((name) => name.startsWith('data-ts-'))).toEqual([]);
    expect(slot.querySelector('.ts-render-badge')).toBeNull();
    expect(document.getElementById('ts-render-trace-panel')).toBeNull();
    expect(renderTrace.diagnostics).toBe(diagnostics);
    renderTrace.dispose();
  });

  it('owns all DOM presentation after activation and releases it without replacing the API', () => {
    const store = new GptDiagnosticsStore({ schedule: (callback) => callback() });
    const renderTrace = createRenderTraceStore();
    const controller = new GptDiagnosticsDataApiController(store, {
      location: window.location,
      schedule: (callback) => {
        callback();
        return () => undefined;
      },
    });
    const api = controller.api;
    const data = Object.freeze({
      api,
      attachPresentation: (factory: GptDiagnosticsPresentationFactory) =>
        controller.attachPresentation(factory),
    });
    const preparationRelease: Array<() => void> = [];
    const activationRelease: Array<() => void> = [];
    const prepared = createDiagnosticsPresentationIntegrationRegistration(RELEASE_ID).prepare(
      Object.freeze({
        config: undefined,
        interfaces: Object.freeze({
          'runtime.v1': Object.freeze({
            boot: () =>
              Object.freeze({
                diagnostics: Object.freeze({
                  version: 1,
                  renderTraceOverlay: false,
                  gpt: Object.freeze({ active: true }),
                }),
              }),
            document,
          }),
          'trace.presentation.v1': Object.freeze({
            attachPresentation: renderTrace.attachPresentation,
          }),
          'gpt_diag.v1': data,
        }),
        signal: new AbortController().signal,
        onDispose: (callback: () => void) => preparationRelease.push(callback),
      } satisfies IntegrationPrepareContext)
    ) as PreparedIntegration;

    expect(controller.api).toBe(api);
    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).toBeNull();
    expect(frames).toEqual([]);

    prepared.activate(
      Object.freeze({
        signal: new AbortController().signal,
        onDispose: (callback: () => void) => activationRelease.push(callback),
        afterCommit: vi.fn(),
      } satisfies IntegrationActivationContext)
    );
    drainFrames();

    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).not.toBeNull();
    expect(controller.api).toBe(api);
    activationRelease.reverse().forEach((release) => release());
    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).toBeNull();
    expect(controller.api).toBe(api);

    preparationRelease.reverse().forEach((release) => release());
    controller.destroy();
    renderTrace.dispose();
  });
});
