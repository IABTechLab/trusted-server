import { describe, expect, it, vi } from 'vitest';

import {
  PROTOCOL_MESSAGE_SCHEMAS_V1,
  TSJS_MESSAGE_PROTOCOL_V1,
  createBrowserMessagingAdapter,
} from '../../src/adapters/messaging';

function createTarget() {
  return {
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  };
}

function createPort() {
  const listeners = new Set<(event: unknown) => void>();
  const messageErrorListeners = new Set<(event: unknown) => void>();
  return {
    addEventListener: vi.fn((type: string, listener: (event: unknown) => void) => {
      (type === 'messageerror' ? messageErrorListeners : listeners).add(listener);
    }),
    close: vi.fn(),
    listeners,
    messageErrorListeners,
    postMessage: vi.fn(),
    removeEventListener: vi.fn((type: string, listener: (event: unknown) => void) => {
      (type === 'messageerror' ? messageErrorListeners : listeners).delete(listener);
    }),
    start: vi.fn(),
  };
}

function createApsRenderer() {
  return {
    type: 'aps',
    version: 1,
    accountId: 'publisher-account',
    bidId: 'bid-1',
    tagType: 'iframe',
    creativeUrl: 'https://creative.example/render',
    width: 300,
    height: 250,
    aaxResponse: 'renderer-envelope',
  };
}

describe('browser messaging adapter', () => {
  it('centralizes every protocol literal and exact message shape as frozen data', () => {
    expect(TSJS_MESSAGE_PROTOCOL_V1).toEqual({
      version: 1,
      rendererVersion: '3',
      message: {
        prebidRequest: 'Prebid Request',
        prebidResponse: 'Prebid Response',
        ownerRegister: 'TS Render Owner Register',
        ownerRegistered: 'TS Render Owner Registered',
        ownerRefused: 'TS Render Owner Refused',
        apsStart: 'TS APS Start',
        admStart: 'TS ADM Start',
        ownerInserted: 'TS Owner Inserted',
        ownerSettled: 'TS Owner Settled',
        admLoaded: 'TS ADM Loaded',
        admFailed: 'TS ADM Failed',
        apsDocumentAccepted: 'TS APS Document Accepted',
        apsRunnerLoaded: 'TS APS Runner Loaded',
        apsRenderCompleted: 'TS APS Render Completed',
        apsRenderFailed: 'TS APS Render Failed',
      },
      status: { ready: 'ready', refused: 'refused' },
      kind: { aps: 'aps', adm: 'adm' },
      outcome: { accepted: 'accepted', failed: 'failed', cancelled: 'cancelled' },
      runnerFailure: {
        descriptorInvalid: 'descriptor_invalid',
        runnerNoLoad: 'runner_no_load',
        runnerFailed: 'runner_failed',
      },
      cancellation: {
        callerAborted: 'caller_aborted',
        superseded: 'superseded',
        navigationDisposed: 'navigation_disposed',
      },
    });
    expect(Object.isFrozen(TSJS_MESSAGE_PROTOCOL_V1)).toBe(true);
    expect(Object.isFrozen(TSJS_MESSAGE_PROTOCOL_V1.message)).toBe(true);
    expect(Object.isFrozen(PROTOCOL_MESSAGE_SCHEMAS_V1)).toBe(true);
    expect(Object.isFrozen(PROTOCOL_MESSAGE_SCHEMAS_V1.apsStart.keys)).toBe(true);
  });

  it('parses global JSON and structured messages through exact descriptor-safe schemas', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    expect(
      adapter.parseProtocolMessage(
        'prebidRequest',
        JSON.stringify({
          message: 'Prebid Request',
          adId: 'r1_1234567890123456789012',
          adServerDomain: 'ads.example.com',
        })
      )
    ).toEqual({
      message: 'Prebid Request',
      adId: 'r1_1234567890123456789012',
      adServerDomain: 'ads.example.com',
    });
    expect(
      adapter.parseProtocolMessage('ownerInserted', {
        message: 'TS Owner Inserted',
        version: 1,
        lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
      })
    ).toEqual({
      message: 'TS Owner Inserted',
      version: 1,
      lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
    });

    for (const candidate of [
      { message: 'TS Owner Inserted', version: 2, lifecycleTicket: 't1_abcdefghijklmnopqrstuv' },
      {
        message: 'TS Owner Inserted',
        version: 1,
        lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
        extra: true,
      },
      { message: 'wrong', version: 1, lifecycleTicket: 't1_abcdefghijklmnopqrstuv' },
      Object.assign(Object.create({ inherited: true }), {
        message: 'TS Owner Inserted',
        version: 1,
        lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
      }),
    ]) {
      expect(adapter.parseProtocolMessage('ownerInserted', candidate)).toBeUndefined();
    }
  });

  it('does not invoke accessors while rejecting an exact-shape candidate', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const getter = vi.fn(() => 'TS Owner Inserted');
    const candidate = {
      version: 1,
      lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
    } as Record<string, unknown>;
    Object.defineProperty(candidate, 'message', { get: getter, enumerable: true });

    expect(adapter.parseProtocolMessage('ownerInserted', candidate)).toBeUndefined();
    expect(getter).not.toHaveBeenCalled();
  });

  it('rejects oversized UTF-8 and duplicate-key global JSON before stateful parsing', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const oversized = JSON.stringify({
      message: 'Prebid Request',
      adId: 'r1_1234567890123456789012',
      adServerDomain: 'é'.repeat(2_100),
    });
    const duplicate =
      '{"message":"Prebid Request","adId":"first","adId":"second","adServerDomain":"ads.example.com"}';

    expect(adapter.parseProtocolMessage('prebidRequest', oversized)).toBeUndefined();
    expect(adapter.parseProtocolMessage('prebidRequest', duplicate)).toBeUndefined();
    expect(
      adapter.parseProtocolMessage('prebidRequest', { message: 'Prebid Request' })
    ).toBeUndefined();
  });

  it('validates capability forms, field types, nested records, enums, and UTF-8 limits', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const request = (adId: unknown, adServerDomain: unknown) =>
      JSON.stringify({ message: 'Prebid Request', adId, adServerDomain });

    expect(
      adapter.parseProtocolMessage(
        'prebidRequest',
        request('r1_abcdefghijklmnopqrstuv', 'é'.repeat(1_024))
      )
    ).toBeDefined();
    for (const candidate of [
      request('r1_too-short', 'ads.example.com'),
      request('a1_abcdefghijklmnopqrstuv', 'ads.example.com'),
      request('r1_abcdefghijklmnopqrstuv', ''),
      request('r1_abcdefghijklmnopqrstuv', 'é'.repeat(1_025)),
      request('r1_abcdefghijklmnopqrstuv', 1),
    ]) {
      expect(adapter.parseProtocolMessage('prebidRequest', candidate)).toBeUndefined();
    }

    expect(
      adapter.parseProtocolMessage('tsOwnerReady', {
        version: 1,
        status: 'ready',
        kind: 'aps',
        lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
      })
    ).toBeDefined();
    expect(
      adapter.parseProtocolMessage('tsOwnerReady', {
        version: 1,
        status: 'ready',
        kind: 'cache',
        lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
      })
    ).toBeUndefined();
    expect(
      adapter.parseProtocolMessage('ownerSettledCancelled', {
        message: 'TS Owner Settled',
        version: 1,
        lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
        outcome: 'cancelled',
        reason: 'external_ready_timeout',
      })
    ).toBeUndefined();
  });

  it('fails APS start closed without exact generation expectations and semantic validation', () => {
    const message = {
      message: 'TS APS Start',
      version: 1,
      lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
      rendererUrl: 'https://publisher.example/integrations/aps/renderer/v1',
      envelope: {
        version: 1,
        nonce: 'n1_abcdefghijklmnopqrstuv',
        publisherOrigin: 'https://publisher.example',
        renderer: Object.freeze(createApsRenderer()),
      },
    };
    expect(
      createBrowserMessagingAdapter(createTarget()).parseProtocolMessage('apsStart', message)
    ).toBeUndefined();

    const adapter = createBrowserMessagingAdapter(createTarget(), {
      expectedPublisherOrigin: 'https://publisher.example',
      expectedRendererUrl: 'https://publisher.example/integrations/aps/renderer/v1',
      validateApsRenderer: () => true,
    });
    expect(adapter.parseProtocolMessage('apsStart', message)).toBeDefined();
    expect(
      adapter.parseProtocolMessage('apsStart', {
        ...message,
        envelope: { ...message.envelope, publisherOrigin: 'https://wrong.example' },
      })
    ).toBeUndefined();
    const throwing = createBrowserMessagingAdapter(createTarget(), {
      expectedPublisherOrigin: 'https://publisher.example',
      expectedRendererUrl: 'https://publisher.example/integrations/aps/renderer/v1',
      validateApsRenderer: () => {
        throw new Error('validator failed');
      },
    });
    expect(() => throwing.parseProtocolMessage('apsStart', message)).not.toThrow();
    expect(throwing.parseProtocolMessage('apsStart', message)).toBeUndefined();
  });

  it('canonicalizes an exact APS renderer before invoking the semantic validator', () => {
    const renderer = createApsRenderer();
    let canonical: unknown;
    const validator = vi.fn((candidate: unknown) => {
      canonical = candidate;
      renderer.bidId = 'mutated-during-validation';
      return true;
    });
    const adapter = createBrowserMessagingAdapter(createTarget(), {
      expectedPublisherOrigin: 'https://publisher.example',
      expectedRendererUrl: 'https://publisher.example/integrations/aps/renderer/v1',
      validateApsRenderer: validator,
    });
    const parsed = adapter.parseProtocolMessage('apsStart', {
      message: 'TS APS Start',
      version: 1,
      lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
      rendererUrl: 'https://publisher.example/integrations/aps/renderer/v1',
      envelope: {
        version: 1,
        nonce: 'n1_abcdefghijklmnopqrstuv',
        publisherOrigin: 'https://publisher.example',
        renderer,
      },
    });

    expect(validator).toHaveBeenCalledTimes(1);
    expect(canonical).not.toBe(renderer);
    expect(Object.getPrototypeOf(canonical)).toBeNull();
    expect(Object.isFrozen(canonical)).toBe(true);
    expect((canonical as Record<string, unknown>)['bidId']).toBe('bid-1');
    expect(
      (parsed?.['envelope'] as Readonly<Record<string, unknown>> | undefined)?.['renderer']
    ).toBe(canonical);
  });

  it('rejects APS renderer accessors, proxies, and unknown keys before validation', () => {
    const validator = vi.fn(() => true);
    const adapter = createBrowserMessagingAdapter(createTarget(), {
      expectedPublisherOrigin: 'https://publisher.example',
      expectedRendererUrl: 'https://publisher.example/integrations/aps/renderer/v1',
      validateApsRenderer: validator,
    });
    const parse = (renderer: unknown) =>
      adapter.parseProtocolMessage('apsEnvelope', {
        version: 1,
        nonce: 'n1_abcdefghijklmnopqrstuv',
        publisherOrigin: 'https://publisher.example',
        renderer,
      });
    const accessor = createApsRenderer();
    const getter = vi.fn(() => 'bid-from-getter');
    Object.defineProperty(accessor, 'bidId', { get: getter, enumerable: true });
    const proxy = new Proxy(createApsRenderer(), {
      ownKeys: () => {
        throw new Error('hostile renderer proxy');
      },
    });

    expect(parse(accessor)).toBeUndefined();
    expect(getter).not.toHaveBeenCalled();
    expect(() => parse(proxy)).not.toThrow();
    expect(parse(proxy)).toBeUndefined();
    expect(parse({ ...createApsRenderer(), unknown: true })).toBeUndefined();
    expect(validator).not.toHaveBeenCalled();
  });

  it('validates both renderer URL expectations and candidates before exact equality', () => {
    const invalidUrls = [
      '/integrations/aps/renderer/v1',
      'ftp://publisher.example/integrations/aps/renderer/v1',
      'https://user@publisher.example/integrations/aps/renderer/v1',
      'https://publisher.example/integrations/aps/renderer/v1?query=1',
      'https://publisher.example/integrations/aps/renderer/v1#fragment',
      'https://publisher.example/wrong-path',
    ];
    for (const invalidUrl of invalidUrls) {
      const adapter = createBrowserMessagingAdapter(createTarget(), {
        expectedPublisherOrigin: 'https://publisher.example',
        expectedRendererUrl: invalidUrl,
        validateApsRenderer: () => true,
      });
      expect(
        adapter.parseProtocolMessage('apsStart', {
          message: 'TS APS Start',
          version: 1,
          lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
          rendererUrl: invalidUrl,
          envelope: {
            version: 1,
            nonce: 'n1_abcdefghijklmnopqrstuv',
            publisherOrigin: 'https://publisher.example',
            renderer: createApsRenderer(),
          },
        })
      ).toBeUndefined();
    }
  });

  it('returns canonical frozen nested records without invoking prototype serialization hooks', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const owner = {
      version: 1,
      status: 'ready',
      kind: 'aps',
      lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
    };
    const toJSON = vi.fn(() => {
      throw new Error('prototype hook called');
    });
    Object.defineProperty(Object.prototype, 'toJSON', { value: toJSON, configurable: true });
    try {
      const parsed = adapter.parseProtocolMessage('prebidResponse', {
        message: 'Prebid Response',
        adId: 'r1_abcdefghijklmnopqrstuv',
        renderer: 'renderer program',
        rendererVersion: '3',
        tsOwner: owner,
      });
      expect(parsed).toBeDefined();
      expect(parsed?.['tsOwner']).not.toBe(owner);
      expect(Object.isFrozen(parsed?.['tsOwner'])).toBe(true);
      owner.kind = 'adm';
      expect(parsed?.['tsOwner']).toMatchObject({ kind: 'aps' });
      expect(toJSON).not.toHaveBeenCalled();
    } finally {
      delete (Object.prototype as { toJSON?: unknown }).toJSON;
    }
  });

  it('parses the renderer-free refused Prebid response as its own exact shape', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const refused = {
      message: 'Prebid Response',
      adId: 'r1_abcdefghijklmnopqrstuv',
      rendererVersion: '3',
      tsOwner: { version: 1, status: 'refused' },
    };
    expect(adapter.parseProtocolMessage('prebidResponseRefused', refused)).toBeDefined();
    expect(
      adapter.parseProtocolMessage('prebidResponseRefused', {
        ...refused,
        renderer: 'must not be present',
      })
    ).toBeUndefined();
    expect(
      adapter.parseProtocolMessage('prebidResponseRefused', {
        ...refused,
        tsOwner: {
          version: 1,
          status: 'ready',
          kind: 'aps',
          lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
        },
      })
    ).toBeUndefined();
  });

  it('returns undefined for an unknown runtime schema kind', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    expect(() =>
      adapter.parseProtocolMessage('unknown' as keyof typeof PROTOCOL_MESSAGE_SCHEMAS_V1, {})
    ).not.toThrow();
    expect(
      adapter.parseProtocolMessage('unknown' as keyof typeof PROTOCOL_MESSAGE_SCHEMAS_V1, {})
    ).toBeUndefined();
  });

  it('extracts exactly zero, one, or two transferred ports into frozen narrow facades', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const first = createPort();
    const second = createPort();

    const zero = adapter.extractTransferredPorts({ ports: [] }, 0);
    const one = adapter.extractTransferredPorts({ ports: [first] }, 1);
    const two = adapter.extractTransferredPorts({ ports: [first, second] }, 2);

    expect(zero).toEqual([]);
    expect(one).toHaveLength(1);
    expect(two).toHaveLength(2);
    expect(Object.isFrozen(zero)).toBe(true);
    expect(Object.isFrozen(one?.[0])).toBe(true);
    expect(one?.[0]).not.toHaveProperty('postMessage');
  });

  it('closes every transferred port on count mismatch and contains hostile closure', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const first = createPort();
    const second = createPort();
    const partial = { close: vi.fn() };
    second.close.mockImplementation(() => {
      throw new Error('close failed');
    });

    expect(() => adapter.extractTransferredPorts({ ports: [first, second] }, 1)).not.toThrow();
    expect(first.close).toHaveBeenCalledTimes(1);
    expect(second.close).toHaveBeenCalledTimes(1);
    expect(() => adapter.extractTransferredPorts({ ports: [partial] }, 0)).not.toThrow();
    expect(partial.close).toHaveBeenCalledTimes(1);
    expect(
      adapter.extractTransferredPorts(
        {
          get ports() {
            throw new Error('hostile');
          },
        },
        0
      )
    ).toBeUndefined();
  });

  it('snapshots hostile transferred-port arrays without accessors, iterators, or duplicate closes', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const first = createPort();
    const hidden = createPort();
    const getter = vi.fn(() => hidden);
    const hostile = [first] as unknown[];
    Object.defineProperty(hostile, '1', { get: getter, enumerable: true });
    Object.defineProperty(hostile, Symbol.iterator, {
      get: () => {
        throw new Error('iterator read');
      },
    });

    expect(() => adapter.extractTransferredPorts({ ports: hostile }, 2)).not.toThrow();
    expect(first.close).toHaveBeenCalledTimes(1);
    expect(getter).not.toHaveBeenCalled();

    const duplicate = createPort();
    expect(adapter.extractTransferredPorts({ ports: [duplicate, duplicate] }, 1)).toBeUndefined();
    expect(duplicate.close).toHaveBeenCalledTimes(1);

    const duplicatePair = createPort();
    expect(
      adapter.extractTransferredPorts({ ports: [duplicatePair, duplicatePair] }, 2)
    ).toBeUndefined();
    expect(duplicatePair.close).toHaveBeenCalledTimes(1);

    const mismatchFirst = createPort();
    const mismatchSecond = createPort();
    expect(
      adapter.extractTransferredPorts({ ports: [mismatchFirst, mismatchSecond, mismatchSecond] }, 0)
    ).toBeUndefined();
    expect(mismatchFirst.close).toHaveBeenCalledTimes(1);
    expect(mismatchSecond.close).toHaveBeenCalledTimes(1);
  });

  it('contains port listener throws and disposes listeners and ports exactly once', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const raw = createPort();
    const [port] = adapter.extractTransferredPorts({ ports: [raw] }, 1) ?? [];
    if (!port) throw new Error('Expected one port');
    const listener = vi.fn(() => {
      throw new Error('listener failed');
    });
    const messageErrorListener = vi.fn(() => {
      throw new Error('messageerror listener failed');
    });
    const unsubscribe = port.listen(listener, messageErrorListener);
    const installed = [...raw.listeners][0];
    const installedMessageError = [...raw.messageErrorListeners][0];

    expect(() => installed?.({ data: { message: 'event' } })).not.toThrow();
    expect(() => installedMessageError?.({ data: 'uncloneable' })).not.toThrow();
    port.post({ message: 'response' }, []);
    unsubscribe();
    unsubscribe();
    port.close();
    port.close();

    expect(listener).toHaveBeenCalledTimes(1);
    expect(messageErrorListener).toHaveBeenCalledTimes(1);
    expect(raw.postMessage).toHaveBeenCalledWith({ message: 'response' }, []);
    expect(raw.removeEventListener).toHaveBeenCalledTimes(2);
    expect(raw.removeEventListener).toHaveBeenCalledWith('message', installed);
    expect(raw.removeEventListener).toHaveBeenCalledWith('messageerror', installedMessageError);
    expect(raw.close).toHaveBeenCalledTimes(1);
  });

  it('rolls back both port listeners when messageerror installation or start fails', () => {
    for (const failure of ['messageerror', 'start'] as const) {
      const adapter = createBrowserMessagingAdapter(createTarget());
      const raw = createPort();
      raw.addEventListener.mockImplementation((type, listener) => {
        (type === 'messageerror' ? raw.messageErrorListeners : raw.listeners).add(listener);
        if (failure === 'messageerror' && type === 'messageerror') {
          throw new Error('messageerror add failed');
        }
      });
      if (failure === 'start') {
        raw.start.mockImplementation(() => {
          throw new Error('start failed');
        });
      }
      const [port] = adapter.extractTransferredPorts({ ports: [raw] }, 1) ?? [];
      if (!port) throw new Error('Expected one port');

      let dispose = (): void => undefined;
      expect(() => {
        dispose = port.listen(vi.fn(), vi.fn());
      }).not.toThrow();
      expect(() => dispose()).not.toThrow();
      expect(raw.listeners.size).toBe(0);
      expect(raw.messageErrorListeners.size).toBe(0);
      expect(raw.removeEventListener).toHaveBeenCalledWith('message', expect.any(Function));
      expect(raw.removeEventListener).toHaveBeenCalledWith('messageerror', expect.any(Function));
      expect(raw.removeEventListener).toHaveBeenCalledTimes(2);
    }
  });

  it.each(['message', 'messageerror', 'start'] as const)(
    'lets reentrant close win during %s port setup',
    (closeDuring) => {
      const adapter = createBrowserMessagingAdapter(createTarget());
      const raw = createPort();
      const [port] = adapter.extractTransferredPorts({ ports: [raw] }, 1) ?? [];
      if (!port) throw new Error('Expected one port');
      raw.addEventListener.mockImplementation((type, listener) => {
        (type === 'messageerror' ? raw.messageErrorListeners : raw.listeners).add(listener);
        if (type === closeDuring) port.close();
      });
      raw.start.mockImplementation(() => {
        if (closeDuring === 'start') port.close();
      });

      const dispose = port.listen(vi.fn(), vi.fn());
      const removals = closeDuring === 'message' ? 1 : 2;

      expect(raw.listeners.size).toBe(0);
      expect(raw.messageErrorListeners.size).toBe(0);
      expect(raw.removeEventListener).toHaveBeenCalledTimes(removals);
      expect(raw.removeEventListener).toHaveBeenCalledWith('message', expect.any(Function));
      if (closeDuring !== 'message') {
        expect(raw.removeEventListener).toHaveBeenCalledWith('messageerror', expect.any(Function));
      }
      if (closeDuring === 'start') expect(raw.start).toHaveBeenCalledTimes(1);
      else expect(raw.start).not.toHaveBeenCalled();
      expect(raw.close).toHaveBeenCalledTimes(1);

      dispose();
      dispose();
      port.close();
      expect(raw.removeEventListener).toHaveBeenCalledTimes(removals);
      expect(raw.close).toHaveBeenCalledTimes(1);
    }
  );

  it('contains hostile capture-target and captured port method throws', () => {
    const installed: Array<(event: MessageEvent) => void> = [];
    const target = {
      addEventListener: vi.fn((_type: 'message', listener: (event: MessageEvent) => void) => {
        installed.push(listener);
      }),
      removeEventListener: vi.fn(() => {
        throw new Error('remove failed');
      }),
    };
    const adapter = createBrowserMessagingAdapter(target);
    const dispose = adapter.installCaptureListener(() => {
      throw new Error('capture failed');
    });
    expect(() => installed[0]?.({} as MessageEvent)).not.toThrow();
    expect(() => dispose()).not.toThrow();

    const raw = createPort();
    raw.postMessage.mockImplementation(() => {
      throw new Error('post failed');
    });
    raw.start.mockImplementation(() => {
      throw new Error('start failed');
    });
    raw.removeEventListener.mockImplementation(() => {
      throw new Error('port remove failed');
    });
    const [port] = adapter.extractTransferredPorts({ ports: [raw] }, 1) ?? [];
    if (!port) throw new Error('Expected one port');
    expect(() => port.post({}, [])).not.toThrow();
    let unsubscribe = (): void => undefined;
    expect(() => {
      unsubscribe = port.listen(vi.fn(), vi.fn());
    }).not.toThrow();
    expect(() => unsubscribe()).not.toThrow();

    const throwingTarget = createBrowserMessagingAdapter({
      addEventListener: () => {
        throw new Error('add failed');
      },
      removeEventListener: vi.fn(),
    });
    expect(() => throwingTarget.installCaptureListener(vi.fn())).not.toThrow();
  });

  it('rolls back the exact capture listener when installation throws after adding it', () => {
    const listeners = new Set<(event: MessageEvent) => void>();
    const removeEventListener = vi.fn(
      (_type: 'message', listener: (event: MessageEvent) => void, _capture: true) => {
        listeners.delete(listener);
      }
    );
    const target = {
      addEventListener: vi.fn(
        (_type: 'message', listener: (event: MessageEvent) => void, _capture: true) => {
          listeners.add(listener);
          throw new Error('add failed after installation');
        }
      ),
      removeEventListener,
    };
    const dispose = createBrowserMessagingAdapter(target).installCaptureListener(vi.fn());
    const installed = target.addEventListener.mock.calls[0]?.[1];

    expect(listeners.size).toBe(0);
    expect(removeEventListener).toHaveBeenCalledTimes(1);
    expect(removeEventListener).toHaveBeenCalledWith('message', installed, true);
    expect(() => dispose()).not.toThrow();
    expect(() => dispose()).not.toThrow();
    expect(removeEventListener).toHaveBeenCalledTimes(1);
  });
});
