import { describe, expect, it, vi } from 'vitest';

import { createBrowserMessagingAdapter } from '../../src/adapters/messaging';
import { createPucBridge } from '../../src/services/puc_bridge';
import type { ReservationRecognition } from '../../src/services/reservations';

const RESERVATION_ID = 'r1_abcdefghijklmnopqrstuv';

function createPort() {
  return {
    addEventListener: vi.fn(),
    close: vi.fn(),
    postMessage: vi.fn(),
    removeEventListener: vi.fn(),
    start: vi.fn(),
  };
}

function exactRequest(adId = RESERVATION_ID): string {
  return JSON.stringify({
    message: 'Prebid Request',
    adId,
    adServerDomain: 'ads.example.com',
  });
}

function createHarness(recognize: (reservationId: unknown) => ReservationRecognition) {
  let listener: ((event: MessageEvent) => void) | undefined;
  const target = {
    addEventListener: vi.fn(
      (_type: 'message', next: (event: MessageEvent) => void, _capture: true) => {
        listener = next;
      }
    ),
    removeEventListener: vi.fn(),
  };
  const bridge = createPucBridge({
    messaging: createBrowserMessagingAdapter(target),
    reservations: { recognize },
  });
  const dispatch = (event: Record<string, unknown>): void => {
    if (!listener) throw new Error('Expected the capture listener to be installed synchronously');
    listener(event as unknown as MessageEvent);
  };
  return { bridge, dispatch, target };
}

describe('Universal Creative bridge dispatcher', () => {
  it('installs one capture listener synchronously and removes only that listener on disposal', () => {
    const harness = createHarness(() => ({ recognized: false }));

    expect(harness.target.addEventListener).toHaveBeenCalledOnce();
    expect(harness.target.addEventListener.mock.calls[0]?.[0]).toBe('message');
    expect(harness.target.addEventListener.mock.calls[0]?.[2]).toBe(true);
    expect(harness.bridge.snapshotInventoryForTest()).toEqual({
      disposed: false,
      pendingClaims: 0,
    });

    harness.bridge.dispose();
    harness.bridge.dispose();
    expect(harness.target.removeEventListener).toHaveBeenCalledOnce();
    expect(harness.target.removeEventListener.mock.calls[0]?.[0]).toBe('message');
    expect(harness.target.removeEventListener.mock.calls[0]?.[2]).toBe(true);
    expect(harness.bridge.snapshotInventoryForTest()).toEqual({
      disposed: true,
      pendingClaims: 0,
    });
  });

  it('leaves native Prebid identifiers untouched before port or source inspection', () => {
    const recognize = vi.fn((): ReservationRecognition => ({ recognized: false }));
    const harness = createHarness(recognize);
    const stopImmediatePropagation = vi.fn();
    const ports = vi.fn(() => {
      throw new Error('native ports must not be read');
    });
    const source = vi.fn(() => {
      throw new Error('native source must not be read');
    });

    harness.dispatch({
      data: exactRequest('native-prebid-id'),
      stopImmediatePropagation,
      get ports() {
        return ports();
      },
      get source() {
        return source();
      },
    });

    expect(recognize).toHaveBeenCalledWith('native-prebid-id');
    expect(stopImmediatePropagation).not.toHaveBeenCalled();
    expect(ports).not.toHaveBeenCalled();
    expect(source).not.toHaveBeenCalled();
    expect(harness.bridge.snapshotInventoryForTest().pendingClaims).toBe(0);
  });

  it.each([
    ['extended object', { message: 'Prebid Request', adId: RESERVATION_ID, extra: true }],
    [
      'extended JSON',
      JSON.stringify({ message: 'Prebid Request', adId: RESERVATION_ID, extra: true }),
    ],
  ])('suppresses and generically refuses a recognized %s before exact parsing', (_label, data) => {
    const order: string[] = [];
    const harness = createHarness((reservationId) => {
      order.push(`lookup:${String(reservationId)}`);
      return { recognized: true, state: 'renderable', expiresAt: 1_000 };
    });
    const port = createPort();

    harness.dispatch({
      data,
      ports: [port],
      source: Object.freeze({}),
      stopImmediatePropagation: vi.fn(() => order.push('stop')),
    });

    expect(order).toEqual([`lookup:${RESERVATION_ID}`, 'stop']);
    expect(port.postMessage).toHaveBeenCalledOnce();
    expect(JSON.parse(String(port.postMessage.mock.calls[0]?.[0]))).toEqual({
      message: 'Prebid Response',
      adId: RESERVATION_ID,
      rendererVersion: '3',
      tsOwner: { version: 1, status: 'refused' },
    });
    expect(port.postMessage.mock.calls[0]?.[1]).toEqual([]);
    expect(port.close).toHaveBeenCalledOnce();
  });

  it('suppresses recognized requests with the wrong port count and closes every available port', () => {
    const harness = createHarness(() => ({
      recognized: true,
      state: 'renderable',
      expiresAt: 1_000,
    }));
    const first = createPort();
    const second = createPort();
    const stopImmediatePropagation = vi.fn();

    harness.dispatch({
      data: exactRequest(),
      ports: [first, second],
      source: Object.freeze({}),
      stopImmediatePropagation,
    });

    expect(stopImmediatePropagation).toHaveBeenCalledOnce();
    expect(first.postMessage).not.toHaveBeenCalled();
    expect(second.postMessage).not.toHaveBeenCalled();
    expect(first.close).toHaveBeenCalledOnce();
    expect(second.close).toHaveBeenCalledOnce();
    expect(harness.bridge.snapshotInventoryForTest().pendingClaims).toBe(0);
  });

  it('buffers only the first exact live claim and generically refuses a duplicate', () => {
    const harness = createHarness(() => ({
      recognized: true,
      state: 'renderable',
      expiresAt: 1_000,
    }));
    const first = createPort();
    const duplicate = createPort();
    const source = Object.freeze({ frame: 'authoritative' });

    harness.dispatch({
      data: exactRequest(),
      ports: [first],
      source,
      stopImmediatePropagation: vi.fn(),
    });

    expect(first.postMessage).not.toHaveBeenCalled();
    expect(first.close).not.toHaveBeenCalled();
    expect(harness.bridge.snapshotInventoryForTest().pendingClaims).toBe(1);

    harness.dispatch({
      data: exactRequest(),
      ports: [duplicate],
      source: Object.freeze({ frame: 'duplicate' }),
      stopImmediatePropagation: vi.fn(),
    });

    expect(duplicate.postMessage).toHaveBeenCalledOnce();
    expect(duplicate.close).toHaveBeenCalledOnce();
    expect(first.postMessage).not.toHaveBeenCalled();
    expect(first.close).not.toHaveBeenCalled();
    expect(harness.bridge.snapshotInventoryForTest().pendingClaims).toBe(1);

    harness.bridge.dispose();
    expect(first.close).toHaveBeenCalledOnce();
    expect(harness.bridge.snapshotInventoryForTest().pendingClaims).toBe(0);
  });

  it.each(['consumed', 'disposed', 'awaiting_prebid_selection'] as const)(
    'suppresses and refuses a recognized non-renderable %s reservation',
    (state) => {
      const harness = createHarness(() => ({ recognized: true, state, expiresAt: 1_000 }));
      const port = createPort();

      harness.dispatch({
        data: exactRequest(),
        ports: [port],
        source: Object.freeze({}),
        stopImmediatePropagation: vi.fn(),
      });

      expect(port.postMessage).toHaveBeenCalledOnce();
      expect(port.close).toHaveBeenCalledOnce();
      expect(harness.bridge.snapshotInventoryForTest().pendingClaims).toBe(0);
    }
  );
});
