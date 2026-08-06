import { describe, expect, it, vi } from 'vitest';

import type { GoogletagAdapter } from '../../src/adapters/googletag';
import type { CaptureMessageListener, MessagingAdapter } from '../../src/adapters/messaging';
import type { PrebidAdapter } from '../../src/adapters/prebid';
import {
  createBrowserComposition,
  createNoopBrowserComposition,
} from '../../src/composition/browser';

function createTarget() {
  return {
    googletag: undefined as unknown,
    pbjs: undefined as unknown,
    addEventListener:
      vi.fn<(type: 'message', listener: CaptureMessageListener, capture: true) => void>(),
    removeEventListener:
      vi.fn<(type: 'message', listener: CaptureMessageListener, capture: true) => void>(),
  };
}

describe('browser composition', () => {
  it('constructs live adapters without changing production globals', () => {
    const target = createTarget();
    const composition = createBrowserComposition({ target });

    expect(composition.adapters.googletag.bindingStatus()).toBe('pending');
    expect(composition.adapters.prebid.bindingStatus()).toBe('pending');
    expect(target.addEventListener).not.toHaveBeenCalled();

    target.googletag = {};
    target.pbjs = {};
    expect(composition.adapters.googletag.bindingStatus()).toBe('present');
    expect(composition.adapters.prebid.bindingStatus()).toBe('present');

    target.googletag = 1;
    target.pbjs = 'not-prebid';
    expect(composition.adapters.googletag.bindingStatus()).toBe('incompatible');
    expect(composition.adapters.prebid.bindingStatus()).toBe('incompatible');
  });

  it('installs the capture-phase message listener synchronously and disposes once', () => {
    const target = createTarget();
    const composition = createBrowserComposition({ target });
    const listener = vi.fn();

    const dispose = composition.adapters.messaging.installCaptureListener(listener);

    expect(target.addEventListener).toHaveBeenCalledTimes(1);
    expect(target.addEventListener).toHaveBeenCalledWith('message', listener, true);

    dispose();
    dispose();
    expect(target.removeEventListener).toHaveBeenCalledTimes(1);
    expect(target.removeEventListener).toHaveBeenCalledWith('message', listener, true);
  });

  it('uses exact injected fakes without constructing concrete adapters', () => {
    const googletag: GoogletagAdapter = {
      bindingStatus: () => 'present',
    };
    const prebid: PrebidAdapter = {
      bindingStatus: () => 'incompatible',
    };
    const messaging: MessagingAdapter = {
      installCaptureListener: () => vi.fn(),
    };

    const composition = createBrowserComposition({
      adapters: { googletag, messaging, prebid },
    });

    expect(composition.adapters).toEqual({ googletag, messaging, prebid });
    expect(Object.isFrozen(composition.adapters)).toBe(true);
    expect(Object.isFrozen(composition)).toBe(true);
  });

  it('provides a side-effect-free no-op composition for kernel and service tests', () => {
    const composition = createNoopBrowserComposition();
    const listener = vi.fn();

    expect(composition.adapters.googletag.bindingStatus()).toBe('pending');
    expect(composition.adapters.prebid.bindingStatus()).toBe('pending');
    expect(() => composition.adapters.messaging.installCaptureListener(listener)()).not.toThrow();
    expect(listener).not.toHaveBeenCalled();
  });
});
