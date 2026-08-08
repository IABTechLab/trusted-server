import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { GoogletagDiagnosticsFact } from '../../../src/adapters/googletag';
import { createGptDiagnosticsFactBuffer } from '../../../src/integrations/gpt_diagnostics/facts';
import { createGptDiagnosticsRuntime } from '../../../src/integrations/gpt_diagnostics';
import { GPT_DIAGNOSTICS_HOST_ID } from '../../../src/integrations/gpt_diagnostics/overlay';

interface FakeSlot {
  getSlotElementId(): string;
  getAdUnitPath(): string;
}

function slot(id: string): FakeSlot {
  return Object.freeze({
    getSlotElementId: () => id,
    getAdUnitPath: () => `/example/site/${id}`,
  });
}

function fact(
  kind: GoogletagDiagnosticsFact['kind'],
  observedSlot: object,
  fields: Partial<GoogletagDiagnosticsFact> = {}
): Readonly<GoogletagDiagnosticsFact> {
  return Object.freeze({ kind, slot: observedSlot, ...fields });
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
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  document.body.replaceChildren();
});

describe('GPT diagnostics runtime', () => {
  it('is inert until activation and publishes no legacy global or mutable authority', () => {
    const buffer = createGptDiagnosticsFactBuffer();
    const runtime = createGptDiagnosticsRuntime(buffer, { window, document });
    const legacyTarget = window as unknown as Record<string, unknown>;

    expect(runtime.currentApi()).toBeUndefined();
    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).toBeNull();

    const release = runtime.activate();
    const api = runtime.currentApi();

    expect(api).toBeDefined();
    expect(Object.isFrozen(api)).toBe(true);
    expect(Reflect.ownKeys(api ?? {}).sort()).toEqual(
      ['export', 'hide', 'show', 'snapshot', 'subscribe'].sort()
    );
    expect(legacyTarget['__tsjs_gpt_diagnostics_active']).toBeUndefined();
    expect(legacyTarget['__tsjs_gpt_diagnostics_runtime']).toBeUndefined();
    expect((legacyTarget['tsjs'] as Record<string, unknown> | undefined)?.['gptDiagnostics']).toBe(
      undefined
    );
    expect(document.querySelectorAll(`#${GPT_DIAGNOSTICS_HOST_ID}`)).toHaveLength(1);

    expect(() => runtime.activate()).toThrow(/already active/i);
    release();
    release();
    expect(runtime.currentApi()).toBeUndefined();
    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).toBeNull();
  });

  it('replays buffered facts and keeps capture active while presentation is hidden', () => {
    const buffer = createGptDiagnosticsFactBuffer();
    const observedSlot = slot('hidden-slot');
    buffer.publish(fact('slotRequested', observedSlot));
    buffer.publish(fact('slotResponseReceived', observedSlot));
    const runtime = createGptDiagnosticsRuntime(buffer, { window, document });

    const release = runtime.activate();
    const api = runtime.currentApi();
    if (!api) throw new Error('Expected active diagnostics API');
    api.hide();
    buffer.publish(
      fact('slotRenderEnded', observedSlot, {
        isEmpty: false,
        size: Object.freeze([300, 250]),
      })
    );

    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).toBeNull();
    expect(api.snapshot().slots[0]?.requests[0]).toMatchObject({
      requestNumber: 1,
      isEmpty: false,
      size: [300, 250],
    });

    api.show();
    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).not.toBeNull();
    release();
  });

  it('releases its consumer so replacement activation receives intervening buffered facts', () => {
    const buffer = createGptDiagnosticsFactBuffer();
    const runtime = createGptDiagnosticsRuntime(buffer, { window, document });
    const firstRelease = runtime.activate();
    firstRelease();
    const observedSlot = slot('replacement-slot');
    buffer.publish(fact('slotRequested', observedSlot));

    const secondRelease = runtime.activate();

    expect(runtime.currentApi()?.snapshot().slots[0]?.slotElementId).toBe('replacement-slot');
    secondRelease();
    buffer.dispose();
  });
});
