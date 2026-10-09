import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { TsjsApi } from '../../../src/core/types';
import * as traceHandoff from '../../../src/trace/handoff';
import {
  installGptDiagnosticsRuntime,
  isGptDiagnosticsActive,
} from '../../../src/integrations/gpt_diagnostics';
import { GPT_DIAGNOSTICS_HOST_ID } from '../../../src/integrations/gpt_diagnostics/overlay';

interface FakeSlot {
  getSlotElementId(): string;
  getAdUnitPath(): string;
}

type Listener = (event: unknown) => void;

type DiagnosticsTestWindow = NonNullable<Parameters<typeof installGptDiagnosticsRuntime>[0]>;

const target = window as unknown as DiagnosticsTestWindow;

function coreApi(): TsjsApi {
  return {
    version: 'test',
    que: [],
    addAdUnits: vi.fn(),
    renderAdUnit: vi.fn(),
    renderAllAdUnits: vi.fn(),
  };
}

function installGptStub() {
  const listeners = new Map<string, Listener[]>();
  const addEventListener = vi.fn((name: string, listener: Listener) => {
    const existing = listeners.get(name) ?? [];
    existing.push(listener);
    listeners.set(name, existing);
  });
  const queue = {
    push: vi.fn((callback: () => void) => {
      callback();
      return 1;
    }),
  };
  target.googletag = {
    cmd: queue,
    pubads: () => ({ addEventListener }),
  };
  return {
    addEventListener,
    queue,
    emit(name: string, event: Record<string, unknown>) {
      for (const listener of listeners.get(name) ?? []) listener(event);
    },
  };
}

function slot(id: string): FakeSlot {
  return {
    getSlotElementId: () => id,
    getAdUnitPath: () => `/example/site/${id}`,
  };
}

async function settle(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

beforeEach(() => {
  document.body.replaceChildren();
  vi.spyOn(document, 'readyState', 'get').mockReturnValue('complete');
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
    callback(0);
    return 1;
  });
  Object.defineProperty(window, 'CSS', {
    configurable: true,
    value: { escape: (value: string) => value },
  });
  target.tsjs = coreApi();
  delete target.googletag;
  delete target.__tsjs_gpt_diagnostics_active;
  delete target.__tsjs_gpt_diagnostics_runtime;
  delete target.__tsjs_trace_active;
});

afterEach(() => {
  target.__tsjs_gpt_diagnostics_runtime?.destroy();
  delete target.__tsjs_gpt_diagnostics_active;
  delete target.__tsjs_gpt_diagnostics_runtime;
  delete target.__tsjs_trace_active;
  delete target.googletag;
  delete target.tsjs;
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  document.body.replaceChildren();
});

describe('GPT diagnostics integration composition', () => {
  it.each([undefined, false, 'true', 1])(
    'installs no trace handoff for nonliteral trace flag %s',
    (active) => {
      const create = vi.spyOn(traceHandoff, 'createTraceHandoff');
      const shadow = vi.spyOn(Element.prototype, 'attachShadow');
      target.__tsjs_gpt_diagnostics_active = true;
      target.__tsjs_trace_active = active;
      installGptStub();
      installGptDiagnosticsRuntime(target);
      const root = shadow.mock.results.at(-1)?.value as ShadowRoot | undefined;
      expect(create).not.toHaveBeenCalled();
      expect(root?.textContent).not.toContain('View trace results');
      expect(target.tsjs?.gptDiagnostics).toBeDefined();
    }
  );
  it('connects the gated trace controls after the public snapshot API and destroys recovery with the runtime', () => {
    const view = vi.fn();
    const download = vi.fn();
    const destroy = vi.fn();
    const create = vi.spyOn(traceHandoff, 'createTraceHandoff').mockImplementation((options) => {
      expect(options.target.tsjs?.gptDiagnostics?.snapshot).toBeTypeOf('function');
      return { view, download, destroy };
    });
    const shadow = vi.spyOn(Element.prototype, 'attachShadow');
    target.__tsjs_gpt_diagnostics_active = true;
    target.__tsjs_trace_active = true;
    installGptStub();
    installGptDiagnosticsRuntime(target);
    const root = shadow.mock.results.at(-1)?.value as ShadowRoot;
    const traceButton = Array.from(root.querySelectorAll('button')).find(
      (item) => item.textContent === 'View trace results'
    );
    expect(traceButton).toBeDefined();
    traceButton?.click();
    expect(view).toHaveBeenCalledTimes(1);
    const options = create.mock.calls[0][0];
    options.onChange?.({ kind: 'storage_unavailable', downloadAvailable: true });
    const recovery = Array.from(root.querySelectorAll('button')).find(
      (item) => item.textContent === 'Download trace report'
    );
    expect(recovery).toBeDefined();
    recovery?.click();
    expect(download).toHaveBeenCalledTimes(1);
    target.__tsjs_gpt_diagnostics_runtime?.destroy();
    expect(destroy).toHaveBeenCalledTimes(1);
    traceButton?.click();
    expect(view).toHaveBeenCalledTimes(1);
  });
  it('has no inactive side effects', () => {
    const originalMutationObserver = window.MutationObserver;

    expect(isGptDiagnosticsActive(target)).toBe(false);
    expect(installGptDiagnosticsRuntime(target)).toBeUndefined();

    expect(target.tsjs?.gptDiagnostics).toBeUndefined();
    expect(target.tsjs?.gptDiagnosticsRecorder).toBeUndefined();
    expect(target.googletag).toBeUndefined();
    expect(target.__tsjs_gpt_diagnostics_runtime).toBeUndefined();
    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).toBeNull();
    expect(window.MutationObserver).toBe(originalMutationObserver);
  });

  it('installs one idempotent active runtime and six listeners', () => {
    target.__tsjs_gpt_diagnostics_active = true;
    const gpt = installGptStub();
    const previousApi = target.tsjs;

    const first = installGptDiagnosticsRuntime(target);
    const second = installGptDiagnosticsRuntime(target);

    expect(first).toBeDefined();
    expect(second).toBe(first);
    expect(target.tsjs).toBe(previousApi);
    expect(target.tsjs?.gptDiagnostics).toBe(first);
    // Evidence writers live on their own channel; the operator API stays read-only.
    expect(Object.keys(first!).sort()).toEqual(['export', 'hide', 'show', 'snapshot', 'subscribe']);
    expect(Object.keys(target.tsjs!.gptDiagnosticsRecorder!).sort()).toEqual([
      'recordPrebidRefresh',
      'recordTrustedServerCreativeFailure',
      'recordTrustedServerCreativeRequest',
      'recordTrustedServerCreativeResponse',
      'recordTrustedServerOpportunity',
    ]);
    expect(gpt.queue.push).toHaveBeenCalledTimes(1);
    expect(gpt.addEventListener).toHaveBeenCalledTimes(6);
    expect(gpt.addEventListener.mock.calls.map(([name]) => name).sort()).toEqual(
      [
        'impressionViewable',
        'slotOnload',
        'slotRenderEnded',
        'slotRequested',
        'slotResponseReceived',
        'slotVisibilityChanged',
      ].sort()
    );
    expect(document.querySelectorAll(`#${GPT_DIAGNOSTICS_HOST_ID}`)).toHaveLength(1);
  });

  it('keeps capture active while presentation is hidden', async () => {
    target.__tsjs_gpt_diagnostics_active = true;
    const gpt = installGptStub();
    const api = installGptDiagnosticsRuntime(target)!;
    const observedSlot = slot('hidden-slot');

    api.hide();
    gpt.emit('slotRequested', { slot: observedSlot });
    gpt.emit('slotResponseReceived', { slot: observedSlot });
    gpt.emit('slotRenderEnded', { slot: observedSlot, isEmpty: false, size: [300, 250] });
    await settle();

    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).toBeNull();
    expect(api.snapshot().slots[0].requests).toHaveLength(1);
    expect(api.snapshot().slots[0].requests[0].isEmpty).toBe(false);

    api.show();
    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).not.toBeNull();
  });

  it('keeps lifecycle, overlap issues, bindings, panel, and export snapshot consistent', async () => {
    target.__tsjs_gpt_diagnostics_active = true;
    const gpt = installGptStub();
    const element = document.createElement('div');
    element.id = 'lifecycle-slot';
    vi.spyOn(element, 'getBoundingClientRect').mockReturnValue({
      left: 20,
      top: 100,
      right: 320,
      bottom: 350,
      width: 300,
      height: 250,
      x: 20,
      y: 100,
      toJSON: () => ({}),
    } as DOMRect);
    document.body.append(element);
    const api = installGptDiagnosticsRuntime(target)!;
    const observedSlot = slot('lifecycle-slot');

    gpt.emit('slotRequested', { slot: observedSlot });
    gpt.emit('slotResponseReceived', { slot: observedSlot });
    gpt.emit('slotRenderEnded', {
      slot: observedSlot,
      isEmpty: false,
      size: [300, 250],
      isBackfill: true,
    });
    gpt.emit('slotOnload', { slot: observedSlot });
    gpt.emit('impressionViewable', { slot: observedSlot });
    gpt.emit('slotVisibilityChanged', { slot: observedSlot, inViewPercentage: 75 });
    gpt.emit('slotRequested', { slot: observedSlot });
    gpt.emit('slotResponseReceived', { slot: observedSlot });
    gpt.emit('slotRenderEnded', { slot: observedSlot, isEmpty: true });
    gpt.emit('slotRequested', { slot: observedSlot });
    gpt.emit('slotRequested', { slot: observedSlot });
    gpt.emit('slotResponseReceived', { slot: observedSlot });
    await settle();

    const snapshot = api.snapshot();
    expect(snapshot.slots).toHaveLength(1);
    expect(snapshot.slots[0]).toMatchObject({
      slotElementId: 'lifecycle-slot',
      adUnitPath: '/example/site/lifecycle-slot',
      binding: { status: 'bound' },
      currentVisibilityPercentage: 75,
    });
    expect(snapshot.slots[0].requests.map((cycle) => cycle.requestNumber)).toEqual([1, 2, 3, 4]);
    expect(snapshot.callbackIssues).toContainEqual(
      expect.objectContaining({
        kind: 'slotResponseReceived',
        disposition: 'ambiguous',
        reason: 'overlapping_request_cycles',
      })
    );
    expect(snapshot.coverage.slotResponseReceived.observed).toBe(
      snapshot.coverage.slotResponseReceived.matched +
        snapshot.coverage.slotResponseReceived.unmatched +
        snapshot.coverage.slotResponseReceived.ambiguous
    );
    expect(document.querySelector(`#${GPT_DIAGNOSTICS_HOST_ID}`)).not.toBeNull();
    expect(document.querySelectorAll(`#${GPT_DIAGNOSTICS_HOST_ID}`)).toHaveLength(1);
    expect(element.getAttributeNames()).toEqual(['id']);
  });

  it('removes both diagnostics channels on teardown', () => {
    target.__tsjs_gpt_diagnostics_active = true;
    installGptStub();
    installGptDiagnosticsRuntime(target);

    expect(target.tsjs?.gptDiagnostics).toBeDefined();
    expect(target.tsjs?.gptDiagnosticsRecorder).toBeDefined();

    target.__tsjs_gpt_diagnostics_runtime!.destroy();

    expect(target.tsjs).toBeDefined();
    expect(target.tsjs?.gptDiagnostics).toBeUndefined();
    expect(target.tsjs?.gptDiagnosticsRecorder).toBeUndefined();
    expect(target.__tsjs_gpt_diagnostics_runtime).toBeUndefined();
  });

  it('leaves no half-initialized API when the core API is unavailable', () => {
    target.__tsjs_gpt_diagnostics_active = true;
    delete target.tsjs;

    expect(installGptDiagnosticsRuntime(target)).toBeUndefined();
    expect(target.__tsjs_gpt_diagnostics_runtime).toBeUndefined();
    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).toBeNull();
  });
});
