import { describe, expect, expectTypeOf, it, vi } from 'vitest';

import type {
  GoogletagAdapter,
  GoogletagDiagnosticsFact,
  GoogletagDiagnosticsObserver,
  GoogletagFacade,
} from '../../../src/adapters/googletag';
import {
  activateGptDiagnosticsFactCapture,
  createGptDiagnosticsFactBuffer,
} from '../../../src/integrations/gpt_diagnostics/facts';

function fact(index: number): Readonly<GoogletagDiagnosticsFact> {
  return Object.freeze({
    kind: 'slotRequested',
    observedAtMs: index,
    slot: Object.freeze({
      token: Object.freeze(Object.create(null) as object),
      elementId: `slot-${index}`,
    }),
  });
}

describe('GPT diagnostics fact transport', () => {
  it('requires diagnostics observation on every GPT adapter', () => {
    expectTypeOf<GoogletagAdapter>().toMatchTypeOf<{
      observeDiagnostics(observer: GoogletagDiagnosticsObserver): (() => void) | undefined;
    }>();
  });

  it('buffers 512 facts, evicts the oldest, replays in order, then releases the buffer', () => {
    const buffer = createGptDiagnosticsFactBuffer();
    for (let index = 0; index < 513; index += 1) expect(buffer.publish(fact(index))).toBe(true);
    const received: number[] = [];

    const release = buffer.activate((item) => {
      received.push(Number(item.slot.elementId?.slice('slot-'.length)));
    });

    expect(received).toHaveLength(512);
    expect(received[0]).toBe(1);
    expect(received[511]).toBe(512);
    expect(buffer.publish(fact(513))).toBe(true);
    expect(received[512]).toBe(513);
    release?.();
    expect(buffer.publish(fact(514))).toBe(true);
    expect(received).toHaveLength(513);
    const replacement = vi.fn();
    expect(buffer.activate(replacement)).toEqual(expect.any(Function));
    expect(replacement).toHaveBeenCalledWith(fact(514));
    buffer.dispose();
    expect(buffer.publish(fact(515))).toBe(false);
  });

  it('isolates consumer throws and admits only one live module consumer', () => {
    const errors: unknown[] = [];
    const buffer = createGptDiagnosticsFactBuffer({
      onConsumerError: (error) => errors.push(error),
    });
    buffer.publish(fact(1));
    const release = buffer.activate(() => {
      throw new Error('fictional consumer failure');
    });

    expect(errors).toHaveLength(1);
    expect(buffer.activate(vi.fn())).toBeUndefined();
    expect(buffer.publish(fact(2))).toBe(true);
    expect(errors).toHaveLength(2);
    release?.();
    expect(buffer.activate(vi.fn())).toEqual(expect.any(Function));
    buffer.dispose();
  });

  it('adds only the four non-correctness GPT listeners while active and disposes all ownership', async () => {
    const subscriptions: string[] = [];
    const releases: Array<ReturnType<typeof vi.fn>> = [];
    let observer: GoogletagDiagnosticsObserver | undefined;
    const operationDispose = vi.fn();
    const facade = Object.freeze({
      subscribe: (eventType: string, _listener: (event: unknown) => void) => {
        subscriptions.push(eventType);
        const release = vi.fn();
        releases.push(release);
        return release;
      },
    }) as unknown as Readonly<GoogletagFacade>;
    const adapter = Object.freeze({
      observeDiagnostics: (candidate: GoogletagDiagnosticsObserver) => {
        observer = candidate;
        return () => {
          observer = undefined;
        };
      },
      run: <Value>(command: (gpt: Readonly<GoogletagFacade>) => Value) =>
        Object.freeze({
          status: 'present' as const,
          result: Promise.resolve(command(facade)),
          dispose: operationDispose,
        }),
    }) as unknown as GoogletagAdapter;
    const buffer = createGptDiagnosticsFactBuffer();

    const dispose = activateGptDiagnosticsFactCapture(adapter, buffer);
    await Promise.resolve();

    expect(observer).toEqual(expect.any(Function));
    expect(subscriptions.sort()).toEqual(
      ['impressionViewable', 'slotOnload', 'slotResponseReceived', 'slotVisibilityChanged'].sort()
    );
    dispose?.();
    dispose?.();
    expect(operationDispose).toHaveBeenCalledOnce();
    expect(releases.every((release) => release.mock.calls.length === 1)).toBe(true);
    expect(observer).toBeUndefined();
  });

  it('rejects capture when another diagnostics observer owns the adapter', () => {
    const run = vi.fn();
    const adapter = Object.freeze({
      observeDiagnostics: () => undefined,
      run,
    }) as unknown as Pick<GoogletagAdapter, 'observeDiagnostics' | 'run'>;

    expect(
      activateGptDiagnosticsFactCapture(adapter, createGptDiagnosticsFactBuffer())
    ).toBeUndefined();
    expect(run).not.toHaveBeenCalled();
  });
});
