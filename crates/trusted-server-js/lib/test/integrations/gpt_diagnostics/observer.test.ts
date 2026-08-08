import { describe, expect, it, vi } from 'vitest';

import type { GoogletagDiagnosticsFact } from '../../../src/adapters/googletag';
import {
  GptDiagnosticsObserver,
  type GptDiagnosticsObserverStore,
  type GptObserverWindow,
} from '../../../src/integrations/gpt_diagnostics/observer';
import type { GptDiagnosticsSlotLike } from '../../../src/integrations/gpt_diagnostics/store';

function fakeStore(): GptDiagnosticsObserverStore {
  return {
    markGptObserved: vi.fn(),
    recordSlotRequested: vi.fn(),
    recordSlotResponseReceived: vi.fn(),
    recordSlotRenderEnded: vi.fn(),
    recordSlotOnload: vi.fn(),
    recordImpressionViewable: vi.fn(),
    recordSlotVisibilityChanged: vi.fn(),
    recordPublisherRefresh: vi.fn(),
  };
}

function fakeSlot(): GptDiagnosticsSlotLike {
  return Object.freeze({
    getSlotElementId: () => 'ad-slot-example',
    getAdUnitPath: () => '/example/site/banner',
  });
}

function fact(
  kind: GoogletagDiagnosticsFact['kind'],
  slot: object,
  fields: Partial<GoogletagDiagnosticsFact> = {}
): Readonly<GoogletagDiagnosticsFact> {
  return Object.freeze({ kind, slot, ...fields });
}

describe('GptDiagnosticsObserver', () => {
  it('starts exactly once without reading or mutating any browser global', () => {
    const store = fakeStore();
    const observer = new GptDiagnosticsObserver(store);

    observer.start();
    observer.start();

    expect(store.markGptObserved).toHaveBeenCalledOnce();
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
    expect(store.recordSlotRequested).toHaveBeenCalledWith(slot);
    expect(store.recordSlotResponseReceived).toHaveBeenCalledWith(slot);
    expect(store.recordSlotRenderEnded).toHaveBeenCalledWith(slot, {
      isEmpty: false,
      size: [300, 250],
      isBackfill: true,
      slotContentChanged: false,
    });
    expect(store.recordSlotOnload).toHaveBeenCalledWith(slot);
    expect(store.recordImpressionViewable).toHaveBeenCalledWith(slot);
    expect(store.recordSlotVisibilityChanged).toHaveBeenCalledWith(slot, 42);
  });

  it('records a malformed visibility fact as unmatched instead of dropping its coverage', () => {
    const store = fakeStore();
    const observer = new GptDiagnosticsObserver(store);

    observer.consume(fact('slotVisibilityChanged', fakeSlot()));

    expect(store.recordSlotVisibilityChanged).toHaveBeenCalledWith(expect.any(Object), NaN);
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
    expect(store.recordSlotOnload).toHaveBeenCalledWith(slot);
  });
});
