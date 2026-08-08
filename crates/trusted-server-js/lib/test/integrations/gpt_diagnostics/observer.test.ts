import { describe, expect, it, vi } from 'vitest';

import type {
  GoogletagDiagnosticsFact,
  GoogletagDiagnosticsSlotSnapshot,
} from '../../../src/adapters/googletag';
import {
  GptDiagnosticsObserver,
  type GptDiagnosticsObserverStore,
} from '../../../src/integrations/gpt_diagnostics/observer';

function fakeStore(): GptDiagnosticsObserverStore {
  return {
    markGptObserved: vi.fn(),
    recordSlotRequested: vi.fn(),
    recordSlotResponseReceived: vi.fn(),
    recordSlotRenderEnded: vi.fn(),
    recordSlotOnload: vi.fn(),
    recordImpressionViewable: vi.fn(),
    recordSlotVisibilityChanged: vi.fn(),
  };
}

function fakeSlot(): GoogletagDiagnosticsSlotSnapshot {
  return Object.freeze({
    token: Object.freeze(Object.create(null) as object),
    elementId: 'ad-slot-example',
    adUnitPath: '/example/site/banner',
  });
}

function fact(
  kind: GoogletagDiagnosticsFact['kind'],
  slot: GoogletagDiagnosticsFact['slot'],
  fields: Partial<GoogletagDiagnosticsFact> = {}
): Readonly<GoogletagDiagnosticsFact> {
  return Object.freeze({ kind, observedAtMs: 1, slot, ...fields });
}

describe('GptDiagnosticsObserver', () => {
  it('does not claim GPT observation merely because the diagnostics module activated', () => {
    const store = fakeStore();
    const observer = new GptDiagnosticsObserver(store);

    observer.start();
    observer.start();

    expect(store.markGptObserved).not.toHaveBeenCalled();
  });

  it('consumes all six normalized adapter facts', () => {
    const store = fakeStore();
    const slot = fakeSlot();
    const observer = new GptDiagnosticsObserver(store);

    observer.consume(fact('slotRequested', slot));
    observer.consume(fact('slotResponseReceived', slot));
    observer.consume(
      fact('slotRenderEnded', slot, {
        isEmpty: false,
        size: Object.freeze([300, 250]),
        isBackfill: true,
        slotContentChanged: false,
      })
    );
    observer.consume(fact('slotOnload', slot));
    observer.consume(fact('impressionViewable', slot));
    observer.consume(fact('slotVisibilityChanged', slot, { inViewPercentage: 42 }));

    expect(store.markGptObserved).toHaveBeenCalledOnce();
    expect(store.recordSlotRequested).toHaveBeenCalledWith(slot, 1);
    expect(store.recordSlotResponseReceived).toHaveBeenCalledWith(slot, 1);
    expect(store.recordSlotRenderEnded).toHaveBeenCalledWith(
      slot,
      {
        isEmpty: false,
        size: [300, 250],
        isBackfill: true,
        slotContentChanged: false,
      },
      1
    );
    expect(store.recordSlotOnload).toHaveBeenCalledWith(slot, 1);
    expect(store.recordImpressionViewable).toHaveBeenCalledWith(slot, 1);
    expect(store.recordSlotVisibilityChanged).toHaveBeenCalledWith(slot, 42, 1);
  });

  it('passes the immutable adapter callback timestamp through to every store mutation', () => {
    const store = fakeStore();
    const observer = new GptDiagnosticsObserver(store);
    const slot = fakeSlot();
    const timestamped = Object.freeze({
      kind: 'slotRequested' as const,
      slot,
      observedAtMs: 123.5,
    });

    observer.consume(timestamped as Readonly<GoogletagDiagnosticsFact>);

    expect(store.recordSlotRequested).toHaveBeenCalledWith(slot, 123.5);
  });

  it('records a malformed visibility fact as unmatched instead of dropping its coverage', () => {
    const store = fakeStore();
    const observer = new GptDiagnosticsObserver(store);

    observer.consume(fact('slotVisibilityChanged', fakeSlot()));

    expect(store.recordSlotVisibilityChanged).toHaveBeenCalledWith(expect.any(Object), NaN, 1);
  });

  it('contains store and logger failures without interrupting later facts', () => {
    const store = fakeStore();
    vi.mocked(store.recordSlotRequested).mockImplementation(() => {
      throw new Error('store failed');
    });
    const logger = {
      warn: vi.fn(() => {
        throw new Error('logger failed');
      }),
    };
    const observer = new GptDiagnosticsObserver(store, { logger });
    const slot = fakeSlot();

    expect(() => observer.consume(fact('slotRequested', slot))).not.toThrow();
    expect(() => observer.consume(fact('slotOnload', slot))).not.toThrow();

    expect(logger.warn).toHaveBeenCalledOnce();
    expect(store.recordSlotOnload).toHaveBeenCalledWith(slot, 1);
  });
});
