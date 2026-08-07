import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { LegacyTsjsApi } from '../../../src/core/types';
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

function coreApi(): LegacyTsjsApi {
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
});

afterEach(() => {
  target.__tsjs_gpt_diagnostics_runtime?.destroy();
  delete target.__tsjs_gpt_diagnostics_active;
  delete target.__tsjs_gpt_diagnostics_runtime;
  delete target.googletag;
  delete target.tsjs;
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  document.body.replaceChildren();
});

describe('GPT diagnostics integration composition', () => {
  it('has no inactive side effects', () => {
    const originalMutationObserver = window.MutationObserver;

    expect(isGptDiagnosticsActive(target)).toBe(false);
    expect(installGptDiagnosticsRuntime(target)).toBeUndefined();

    expect(target.tsjs?.gptDiagnostics).toBeUndefined();
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
    expect(api.snapshot().slots[0]!.requests).toHaveLength(1);
    expect(api.snapshot().slots[0]!.requests[0]!.isEmpty).toBe(false);

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
    expect(snapshot.slots[0]!.requests.map((cycle) => cycle.requestNumber)).toEqual([1, 2, 3, 4]);
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

  it('leaves no half-initialized API when the core API is unavailable', () => {
    target.__tsjs_gpt_diagnostics_active = true;
    delete target.tsjs;

    expect(installGptDiagnosticsRuntime(target)).toBeUndefined();
    expect(target.__tsjs_gpt_diagnostics_runtime).toBeUndefined();
    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).toBeNull();
  });
});
