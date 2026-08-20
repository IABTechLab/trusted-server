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
  it('creates one owned channel and transfers only its exact wrapped endpoint', () => {
    const retainedRaw = createPort();
    const transferredRaw = createPort();
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const channel = adapter.createChannel();
    if (!channel) throw new Error('Expected one channel');
    expect(Object.isFrozen(channel)).toBe(true);
    expect(Object.isFrozen(channel.retained)).toBe(true);
    expect(Object.isFrozen(channel.transferred)).toBe(true);

    const receiver = { postMessage: vi.fn() };
    const envelope = Object.freeze({ version: 1, nonce: 'n1_abcdefghijklmnopqrstuv' });
    expect(adapter.postWindow(receiver, envelope, '*', [channel.transferred])).toBe(true);
    expect(receiver.postMessage).toHaveBeenCalledWith(envelope, '*', [transferredRaw]);
    channel.transferred.close();
    expect(transferredRaw.close).not.toHaveBeenCalled();
    expect(adapter.postWindow(receiver, envelope, '*', [channel.transferred])).toBe(false);
    channel.retained.close();
    expect(retainedRaw.close).toHaveBeenCalledOnce();
  });

  it('leaves an untransferred endpoint locally closeable when exact window posting fails', () => {
    const retainedRaw = createPort();
    const transferredRaw = createPort();
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const channel = adapter.createChannel();
    if (!channel) throw new Error('Expected one channel');
    const receiver = {
      postMessage: vi.fn(() => {
        throw new Error('window post failed');
      }),
    };
    expect(adapter.postWindow(receiver, Object.freeze({}), '*', [channel.transferred])).toBe(false);
    channel.transferred.close();
    expect(transferredRaw.close).toHaveBeenCalledOnce();
    channel.retained.close();
  });

  it('reserves a transferred endpoint before a reentrant window post', () => {
    const retainedRaw = createPort();
    const transferredRaw = createPort();
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const channel = adapter.createChannel();
    if (!channel) throw new Error('Expected one channel');
    let nested: boolean | undefined;
    const receiver = {
      postMessage: vi.fn(() => {
        nested = adapter.postWindow(receiver, Object.freeze({ nested: true }), '*', [
          channel.transferred,
        ]);
      }),
    };
    expect(
      adapter.postWindow(receiver, Object.freeze({ outer: true }), '*', [channel.transferred])
    ).toBe(true);
    expect(nested).toBe(false);
    expect(receiver.postMessage).toHaveBeenCalledOnce();
    channel.transferred.close();
    expect(transferredRaw.close).not.toHaveBeenCalled();
  });

  it('closes invalid channel endpoints without returning a partial facade', () => {
    const duplicate = createPort();
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1 = duplicate;
        readonly port2 = duplicate;
      },
    });
    expect(adapter.createChannel()).toBeUndefined();
    expect(duplicate.close).toHaveBeenCalledOnce();
    expect(createBrowserMessagingAdapter(createTarget()).createChannel()).toBeUndefined();
  });

  it('transfers through descriptor snapshots when Array prototype operations are poisoned', () => {
    const retainedRaw = createPort();
    const transferredRaw = createPort();
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const channel = adapter.createChannel();
    if (!channel) throw new Error('Expected one channel');
    let posts = 0;
    let receivedTransfer: unknown;
    const receiver = {
      postMessage(...parameters: unknown[]): void {
        posts += 1;
        receivedTransfer = parameters[2];
      },
    };
    const originalFilter = Object.getOwnPropertyDescriptor(Array.prototype, 'filter');
    const originalSort = Object.getOwnPropertyDescriptor(Array.prototype, 'sort');
    const originalPush = Object.getOwnPropertyDescriptor(Array.prototype, 'push');
    const originalIterator = Object.getOwnPropertyDescriptor(Array.prototype, Symbol.iterator);
    const poisoned = (): never => {
      throw new Error('poisoned Array prototype operation');
    };
    let result: boolean | undefined;
    let thrown: unknown;
    try {
      Object.defineProperty(Array.prototype, 'filter', { value: poisoned, configurable: true });
      Object.defineProperty(Array.prototype, 'sort', { value: poisoned, configurable: true });
      Object.defineProperty(Array.prototype, 'push', { value: poisoned, configurable: true });
      Object.defineProperty(Array.prototype, Symbol.iterator, {
        value: poisoned,
        configurable: true,
      });
      result = adapter.postWindow(receiver, Object.freeze({}), '*', [channel.transferred]);
    } catch (error) {
      thrown = error;
    } finally {
      if (originalFilter) Object.defineProperty(Array.prototype, 'filter', originalFilter);
      if (originalSort) Object.defineProperty(Array.prototype, 'sort', originalSort);
      if (originalPush) Object.defineProperty(Array.prototype, 'push', originalPush);
      if (originalIterator) {
        Object.defineProperty(Array.prototype, Symbol.iterator, originalIterator);
      }
    }

    expect(thrown).toBeUndefined();
    expect(result).toBe(true);
    expect(posts).toBe(1);
    expect(receivedTransfer).toEqual([transferredRaw]);
    channel.retained.close();
  });

  it('drains listeners and closes the raw port when collection iterators are poisoned', () => {
    let messageListener: unknown;
    let messageErrorListener: unknown;
    let removals = 0;
    let closes = 0;
    const raw = {
      addEventListener(type: string, listener: unknown): void {
        if (type === 'message') messageListener = listener;
        else messageErrorListener = listener;
      },
      close(): void {
        closes += 1;
      },
      postMessage(): void {},
      removeEventListener(type: string, listener: unknown): void {
        if (type === 'message' && listener === messageListener) removals += 1;
        if (type === 'messageerror' && listener === messageErrorListener) removals += 1;
      },
      start(): void {},
    };
    const adapter = createBrowserMessagingAdapter(createTarget());
    const [port] = adapter.extractTransferredPorts({ ports: [raw] }, 1) ?? [];
    if (!port) throw new Error('Expected one port');
    port.listen(
      () => undefined,
      () => undefined
    );

    const originalSetIterator = Object.getOwnPropertyDescriptor(Set.prototype, Symbol.iterator);
    const originalSetValues = Object.getOwnPropertyDescriptor(Set.prototype, 'values');
    const originalArrayIterator = Object.getOwnPropertyDescriptor(Array.prototype, Symbol.iterator);
    const iteratorPrototype = Object.getPrototypeOf(new Set().values()) as object;
    const originalNext = Object.getOwnPropertyDescriptor(iteratorPrototype, 'next');
    const poisoned = (): never => {
      throw new Error('poisoned collection iterator');
    };
    let thrown: unknown;
    try {
      Object.defineProperty(Set.prototype, Symbol.iterator, {
        value: poisoned,
        configurable: true,
      });
      Object.defineProperty(Set.prototype, 'values', { value: poisoned, configurable: true });
      Object.defineProperty(Array.prototype, Symbol.iterator, {
        value: poisoned,
        configurable: true,
      });
      Object.defineProperty(iteratorPrototype, 'next', { value: poisoned, configurable: true });
      port.close();
    } catch (error) {
      thrown = error;
    } finally {
      if (originalSetIterator) {
        Object.defineProperty(Set.prototype, Symbol.iterator, originalSetIterator);
      }
      if (originalSetValues) Object.defineProperty(Set.prototype, 'values', originalSetValues);
      if (originalArrayIterator) {
        Object.defineProperty(Array.prototype, Symbol.iterator, originalArrayIterator);
      }
      if (originalNext) Object.defineProperty(iteratorPrototype, 'next', originalNext);
    }

    expect(thrown).toBeUndefined();
    expect(removals).toBe(2);
    expect(closes).toBe(1);
    port.close();
    expect(closes).toBe(1);
  });

  it('posts through a wrapped port without dynamic transfer-array iteration', () => {
    const raw = createPort();
    const adapter = createBrowserMessagingAdapter(createTarget());
    const [port] = adapter.extractTransferredPorts({ ports: [raw] }, 1) ?? [];
    if (!port) throw new Error('Expected one port');
    const transferred: unknown[] = [];
    const originalIterator = Object.getOwnPropertyDescriptor(Array.prototype, Symbol.iterator);
    const poisoned = (): never => {
      throw new Error('poisoned Array iterator');
    };
    try {
      Object.defineProperty(Array.prototype, Symbol.iterator, {
        value: poisoned,
        configurable: true,
      });
      port.post(Object.freeze({ message: true }), transferred);
    } finally {
      if (originalIterator) {
        Object.defineProperty(Array.prototype, Symbol.iterator, originalIterator);
      }
    }

    expect(raw.postMessage).toHaveBeenCalledWith({ message: true }, []);
    port.close();
  });

  it('unwraps and commits exact channel endpoints transferred through a retained port', () => {
    const retainedRaw = createPort();
    const transferredRaw = createPort();
    const controlRaw = createPort();
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const channel = adapter.createChannel();
    const [control] = adapter.extractTransferredPorts({ ports: [controlRaw] }, 1) ?? [];
    if (!channel || !control) throw new Error('Expected channel and control port');
    const message = Object.freeze({ message: 'transfer' });

    expect(control.post(message, [channel.transferred])).toBe(true);
    expect(controlRaw.postMessage).toHaveBeenCalledWith(message, [transferredRaw]);
    expect(control.post(message, [channel.transferred])).toBe(false);
    channel.transferred.close();
    expect(transferredRaw.close).not.toHaveBeenCalled();
    channel.retained.close();
    control.close();
  });

  it('rolls back exact channel transfer ownership when retained-port posting throws', () => {
    const retainedRaw = createPort();
    const transferredRaw = createPort();
    const controlRaw = createPort();
    controlRaw.postMessage.mockImplementation(() => {
      throw new Error('port post failed');
    });
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const channel = adapter.createChannel();
    const [control] = adapter.extractTransferredPorts({ ports: [controlRaw] }, 1) ?? [];
    if (!channel || !control) throw new Error('Expected channel and control port');

    expect(control.post(Object.freeze({}), [channel.transferred])).toBe(false);
    channel.transferred.close();
    expect(transferredRaw.close).toHaveBeenCalledOnce();
    channel.retained.close();
    control.close();
  });

  it('uses captured close authority when later channel validation fails', () => {
    let closeReads = 0;
    let closes = 0;
    const first = {
      addEventListener(): void {},
      get close(): () => void {
        closeReads += 1;
        if (closeReads > 1) throw new Error('close authority re-read');
        return () => {
          closes += 1;
        };
      },
      postMessage(): void {},
      removeEventListener(): void {},
      start(): void {},
    };
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1 = first;
        readonly port2 = Object.freeze({ invalid: true });
      },
    });

    expect(adapter.createChannel()).toBeUndefined();
    expect(closeReads).toBe(1);
    expect(closes).toBe(1);
  });

  it('preserves captured close authority when later raw-port method inspection throws', () => {
    const first = createPort();
    let closeReads = 0;
    let closes = 0;
    const partial = {
      addEventListener(): void {},
      get close(): () => void {
        closeReads += 1;
        if (closeReads > 1) throw new Error('close authority re-read');
        return () => {
          closes += 1;
        };
      },
      get postMessage(): never {
        throw new Error('later port inspection failed');
      },
      removeEventListener(): void {},
      start(): void {},
    };
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1 = first;
        readonly port2 = partial;
      },
    });

    expect(adapter.createChannel()).toBeUndefined();
    expect(closeReads).toBe(1);
    expect(closes).toBe(1);
    expect(first.close).toHaveBeenCalledOnce();
  });

  it('captures the first endpoint close before a hostile second-endpoint getter runs', () => {
    let closeReads = 0;
    let poisonedCloseReads = 0;
    let closes = 0;
    const first = {
      addEventListener(): void {},
      get close(): () => void {
        closeReads += 1;
        if (closeReads > 1) throw new Error('first close authority re-read');
        return () => {
          closes += 1;
        };
      },
      postMessage(): void {},
      removeEventListener(): void {},
      start(): void {},
    };
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1 = first;

        get port2(): never {
          Object.defineProperty(first, 'close', {
            configurable: true,
            get: () => {
              poisonedCloseReads += 1;
              throw new Error('first close authority poisoned');
            },
          });
          throw new Error('second endpoint unavailable');
        }
      },
    });

    expect(adapter.createChannel()).toBeUndefined();
    expect(closeReads).toBe(1);
    expect(poisonedCloseReads).toBe(0);
    expect(closes).toBe(1);
  });

  it('uses captured WeakMap authority for channel registration and facade lookup', () => {
    const retainedRaw = createPort();
    const transferredRaw = createPort();
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const originalGet = WeakMap.prototype.get;
    const originalSet = WeakMap.prototype.set;
    let dynamicGets = 0;
    let dynamicSets = 0;
    WeakMap.prototype.set = function <K extends WeakKey, V>(
      this: WeakMap<K, V>,
      key: K,
      value: V
    ): WeakMap<K, V> {
      dynamicSets += 1;
      Reflect.apply(originalSet, this, [key, value]);
      throw new Error('registration intercepted');
    };
    WeakMap.prototype.get = function <K extends WeakKey, V>(this: WeakMap<K, V>, _key: K): V {
      dynamicGets += 1;
      return {
        raw: { binding: transferredRaw },
        transferable: true,
        closed: false,
        transferred: false,
        transferring: false,
      } as V;
    };

    let channel: ReturnType<typeof adapter.createChannel>;
    let forgedResult: boolean | undefined;
    let thrown: unknown;
    let posts = 0;
    try {
      channel = adapter.createChannel();
      forgedResult = adapter.postWindow(
        { postMessage: () => (posts += 1) },
        Object.freeze({}),
        '*',
        [Object.freeze({}) as never]
      );
    } catch (error) {
      thrown = error;
    } finally {
      WeakMap.prototype.get = originalGet;
      WeakMap.prototype.set = originalSet;
    }

    expect(thrown).toBeUndefined();
    expect(channel).toBeDefined();
    expect(forgedResult).toBe(false);
    expect(posts).toBe(0);
    expect(dynamicGets).toBe(0);
    expect(dynamicSets).toBe(0);
    channel?.retained.close();
    channel?.transferred.close();
  });

  it('rejects MessageChannel constructors that reuse an already-owned raw endpoint', () => {
    const retainedRaw = createPort();
    const transferredRaw = createPort();
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const first = adapter.createChannel();
    if (!first) throw new Error('Expected one channel');

    expect(adapter.createChannel()).toBeUndefined();
    expect(retainedRaw.close).not.toHaveBeenCalled();
    expect(transferredRaw.close).not.toHaveBeenCalled();
    first.retained.close();
    first.transferred.close();
    expect(retainedRaw.close).toHaveBeenCalledOnce();
    expect(transferredRaw.close).toHaveBeenCalledOnce();
  });

  it('does not close a live owned endpoint when a later constructor returns it twice', () => {
    const retainedRaw = createPort();
    const transferredRaw = createPort();
    let constructions = 0;
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1: unknown;
        readonly port2: unknown;

        constructor() {
          constructions += 1;
          this.port1 = retainedRaw;
          this.port2 = constructions === 1 ? transferredRaw : retainedRaw;
        }
      },
    });
    const first = adapter.createChannel();
    if (!first) throw new Error('Expected one channel');

    expect(adapter.createChannel()).toBeUndefined();
    expect(retainedRaw.close).not.toHaveBeenCalled();
    expect(transferredRaw.close).not.toHaveBeenCalled();
    first.retained.close();
    first.transferred.close();
    expect(retainedRaw.close).toHaveBeenCalledOnce();
    expect(transferredRaw.close).toHaveBeenCalledOnce();
  });

  it('keeps failed channel bindings retired throughout reentrant close cleanup', () => {
    let constructions = 0;
    let closes = 0;
    let nestedCloses = 0;
    let nestedResult: ReturnType<ReturnType<typeof createBrowserMessagingAdapter>['createChannel']>;
    const retainedRaw = {
      addEventListener(): void {},
      close(): void {
        closes += 1;
        nestedResult = adapter.createChannel();
      },
      postMessage(): void {},
      removeEventListener(): void {},
      start(): void {},
    };
    const nestedRaw = {
      addEventListener(): void {},
      close(): void {
        nestedCloses += 1;
      },
      postMessage(): void {},
      removeEventListener(): void {},
      start(): void {},
    };
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1: unknown;
        readonly port2: unknown;

        constructor() {
          constructions += 1;
          this.port1 = retainedRaw;
          this.port2 = constructions === 1 ? Object.freeze({ invalid: true }) : nestedRaw;
        }
      },
    });

    expect(adapter.createChannel()).toBeUndefined();
    expect(nestedResult).toBeUndefined();
    expect(closes).toBe(1);
    expect(nestedCloses).toBe(1);
  });

  it('rejects channel-owned raw endpoints at transferred-port extraction without closing them', () => {
    const retainedRaw = createPort();
    const transferredRaw = createPort();
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const channel = adapter.createChannel();
    if (!channel) throw new Error('Expected one channel');

    expect(adapter.extractTransferredPorts({ ports: [retainedRaw] }, 1)).toBeUndefined();
    expect(adapter.extractTransferredPorts({ ports: [retainedRaw] }, 0)).toBeUndefined();
    expect(retainedRaw.close).not.toHaveBeenCalled();
    channel.retained.close();
    channel.transferred.close();
    expect(retainedRaw.close).toHaveBeenCalledOnce();
    expect(transferredRaw.close).toHaveBeenCalledOnce();
  });

  it('rejects MessageChannel endpoints already owned by transferred-port extraction', () => {
    const extractedRaw = createPort();
    const newRaw = createPort();
    const adapter = createBrowserMessagingAdapter({
      ...createTarget(),
      MessageChannel: class {
        readonly port1 = extractedRaw;
        readonly port2 = newRaw;
      },
    });
    const [extracted] = adapter.extractTransferredPorts({ ports: [extractedRaw] }, 1) ?? [];
    if (!extracted) throw new Error('Expected one extracted port');

    expect(adapter.createChannel()).toBeUndefined();
    expect(extractedRaw.close).not.toHaveBeenCalled();
    expect(newRaw.close).toHaveBeenCalledOnce();
    extracted.close();
    expect(extractedRaw.close).toHaveBeenCalledOnce();
  });

  it('extracts and wraps transferred ports without dynamic Array operations or iteration', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const raw = createPort();
    const originalPush = Object.getOwnPropertyDescriptor(Array.prototype, 'push');
    const originalIterator = Object.getOwnPropertyDescriptor(Array.prototype, Symbol.iterator);
    const poisoned = (): never => {
      throw new Error('poisoned Array operation');
    };
    let extracted: readonly unknown[] | undefined;
    let thrown: unknown;
    try {
      Object.defineProperty(Array.prototype, 'push', { value: poisoned, configurable: true });
      Object.defineProperty(Array.prototype, Symbol.iterator, {
        value: poisoned,
        configurable: true,
      });
      extracted = adapter.extractTransferredPorts({ ports: [raw] }, 1);
    } catch (error) {
      thrown = error;
    } finally {
      if (originalPush) Object.defineProperty(Array.prototype, 'push', originalPush);
      if (originalIterator) {
        Object.defineProperty(Array.prototype, Symbol.iterator, originalIterator);
      }
    }

    expect(thrown).toBeUndefined();
    expect(extracted).toHaveLength(1);
    const port = extracted?.[0] as { close?: () => void } | undefined;
    port?.close?.();
    expect(raw.close).toHaveBeenCalledOnce();
  });

  it('centralizes every protocol literal and exact message shape as frozen data', () => {
    expect(TSJS_MESSAGE_PROTOCOL_V1).toEqual({
      version: 1,
      rendererVersion: '4',
      message: {
        prebidRequest: 'Prebid Request',
        prebidResponse: 'Prebid Response',
        ownerRegister: 'TS Render Owner Register',
        ownerRegistered: 'TS Render Owner Registered',
        ownerRefused: 'TS Render Owner Refused',
        apsTopMountStarted: 'TS APS Top Mount Started',
        admStart: 'TS ADM Start',
        ownerInserted: 'TS Owner Inserted',
        ownerSettled: 'TS Owner Settled',
        admLoaded: 'TS ADM Loaded',
        admFailed: 'TS ADM Failed',
        apsBootstrapReady: 'TS APS Bootstrap Ready',
        apsBootstrapConfigure: 'TS APS Bootstrap Configure',
        apsInnerReady: 'TS APS Inner Ready',
        apsInnerBind: 'TS APS Inner Bind',
        apsContainerReady: 'TS APS Container Ready',
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
    expect(Object.isFrozen(PROTOCOL_MESSAGE_SCHEMAS_V1.apsTopMountStarted.keys)).toBe(true);
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

  it('parses the exact bounded bootstrap, inner, and container window channels', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const bootstrapNonce = `b1_${'b'.repeat(22)}`;
    const rendererNonce = `n1_${'n'.repeat(22)}`;
    const creativeOrigin = 'https://creative.example';
    const messages = [
      ['apsBootstrapReady', { message: 'TS APS Bootstrap Ready', version: 1, bootstrapNonce }],
      [
        'apsBootstrapConfigure',
        {
          message: 'TS APS Bootstrap Configure',
          version: 2,
          bootstrapNonce,
          rendererNonce,
          creativeOrigin,
          tagType: 'iframe',
        },
      ],
      ['apsInnerReady', { message: 'TS APS Inner Ready', version: 1, rendererNonce }],
      ['apsInnerBind', { message: 'TS APS Inner Bind', version: 1, rendererNonce }],
      [
        'apsContainerReady',
        {
          message: 'TS APS Container Ready',
          version: 1,
          bootstrapNonce,
          rendererNonce,
        },
      ],
    ] as const;
    for (const [kind, value] of messages) {
      expect(adapter.parseProtocolMessage(kind, JSON.stringify(value))).toEqual(value);
      expect(
        adapter.parseProtocolMessage(kind, JSON.stringify({ ...value, extra: true }))
      ).toBeUndefined();
    }
    expect(
      adapter.parseProtocolMessage(
        'apsBootstrapConfigure',
        `{"message":"TS APS Bootstrap Configure","version":2,"bootstrapNonce":"${bootstrapNonce}","bootstrapNonce":"${bootstrapNonce}","rendererNonce":"${rendererNonce}","creativeOrigin":"${creativeOrigin}","tagType":"iframe"}`
      )
    ).toBeUndefined();
    expect(
      adapter.parseProtocolMessage(
        'apsBootstrapConfigure',
        JSON.stringify({
          message: 'TS APS Bootstrap Configure',
          version: 2,
          bootstrapNonce,
          rendererNonce,
          creativeOrigin: 'http://creative.example',
          tagType: 'iframe',
        })
      )
    ).toBeUndefined();
    expect(
      adapter.parseProtocolMessage(
        'apsBootstrapConfigure',
        JSON.stringify({
          message: 'TS APS Bootstrap Configure',
          version: 2,
          bootstrapNonce,
          rendererNonce,
          creativeOrigin,
          tagType: 'script',
        })
      )
    ).toBeDefined();
    expect(
      adapter.parseProtocolMessage(
        'apsBootstrapConfigure',
        JSON.stringify({
          message: 'TS APS Bootstrap Configure',
          version: 2,
          bootstrapNonce,
          rendererNonce,
          creativeOrigin,
          tagType: 'image',
        })
      )
    ).toBeUndefined();
  });

  it('inspects only own routing data before exact global-message parsing', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const json = JSON.stringify({
      message: 'Prebid Request',
      adId: 'r1_abcdefghijklmnopqrstuv',
      adServerDomain: 'ads.example.com',
      ignored: { renderer: '<script>not routing data</script>' },
    });
    const object = Object.assign(Object.create(null), {
      message: 'TS Render Owner Register',
      adId: 'r1_abcdefghijklmnopqrstuv',
      lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
      ignored: true,
    });

    const inspectedJson = adapter.inspectGlobalMessage(json);
    const inspectedObject = adapter.inspectGlobalMessage(object);

    expect(inspectedJson).toEqual({
      message: 'Prebid Request',
      adId: 'r1_abcdefghijklmnopqrstuv',
    });
    expect(inspectedObject).toEqual({
      message: 'TS Render Owner Register',
      adId: 'r1_abcdefghijklmnopqrstuv',
      lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
    });
    expect(Object.isFrozen(inspectedJson)).toBe(true);
    expect(Object.isFrozen(inspectedObject)).toBe(true);
  });

  it('inspects global routing data without invoking accessors or inherited properties', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const getter = vi.fn(() => 'Prebid Request');
    const accessor = Object.create(null) as Record<string, unknown>;
    Object.defineProperty(accessor, 'message', { get: getter, enumerable: true });
    Object.defineProperty(accessor, 'adId', {
      value: 'r1_abcdefghijklmnopqrstuv',
      enumerable: true,
    });
    const inherited = Object.assign(Object.create({ message: 'Prebid Request' }), {
      adId: 'r1_abcdefghijklmnopqrstuv',
    });
    const throwingProxy = new Proxy(
      {},
      {
        getPrototypeOf: () => {
          throw new Error('prototype trap');
        },
      }
    );

    expect(adapter.inspectGlobalMessage(accessor)).toBeUndefined();
    expect(adapter.inspectGlobalMessage(inherited)).toBeUndefined();
    expect(adapter.inspectGlobalMessage(throwingProxy)).toBeUndefined();
    expect(getter).not.toHaveBeenCalled();
  });

  it('rejects malformed, duplicate-key, and oversized routing JSON during inspection', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const duplicate = '{"message":"Prebid Request","adId":"first","adId":"second","ignored":true}';
    const oversized = JSON.stringify({
      message: 'Prebid Request',
      adId: 'r1_abcdefghijklmnopqrstuv',
      ignored: 'é'.repeat(2_100),
    });

    expect(adapter.inspectGlobalMessage('{')).toBeUndefined();
    expect(adapter.inspectGlobalMessage(duplicate)).toBeUndefined();
    expect(adapter.inspectGlobalMessage(oversized)).toBeUndefined();
    expect(adapter.inspectGlobalMessage({ adId: 'r1_abcdefghijklmnopqrstuv' })).toBeUndefined();
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

  it.each(['before', 'after'] as const)(
    'fails global JSON parsing closed when duplicate-key tracking throws %s insertion',
    (failure) => {
      const adapter = createBrowserMessagingAdapter(createTarget());
      const originalSetAdd = Set.prototype.add;
      Set.prototype.add = function (this: Set<unknown>, value: unknown): Set<unknown> {
        if (failure === 'after') Reflect.apply(originalSetAdd, this, [value]);
        throw new Error(`duplicate-key tracking failed ${failure} insertion`);
      } as typeof Set.prototype.add;

      let parsed: unknown;
      let thrown: unknown;
      try {
        parsed = adapter.parseProtocolMessage(
          'prebidRequest',
          JSON.stringify({
            message: 'Prebid Request',
            adId: 'r1_1234567890123456789012',
            adServerDomain: 'ads.example.com',
          })
        );
      } catch (error) {
        thrown = error;
      } finally {
        Set.prototype.add = originalSetAdd;
      }

      expect(thrown).toBeUndefined();
      expect(parsed).toBeUndefined();
    }
  );

  it.each(['duplicate-key', 'reason'] as const)(
    'fails protocol %s membership checks closed when Set.has throws',
    (lookup) => {
      const adapter = createBrowserMessagingAdapter(createTarget());
      const candidate =
        lookup === 'duplicate-key'
          ? JSON.stringify({
              message: 'Prebid Request',
              adId: 'r1_1234567890123456789012',
              adServerDomain: 'ads.example.com',
            })
          : {
              message: 'TS Owner Settled',
              version: 1,
              lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
              outcome: 'failed',
              reason: 'internal_error',
            };
      const originalSetHas = Set.prototype.has;
      Set.prototype.has = function (): boolean {
        throw new Error(`${lookup} membership failed`);
      } as typeof Set.prototype.has;

      let parsed: unknown;
      let thrown: unknown;
      try {
        parsed = adapter.parseProtocolMessage(
          lookup === 'duplicate-key' ? 'prebidRequest' : 'ownerSettledFailed',
          candidate
        );
      } catch (error) {
        thrown = error;
      } finally {
        Set.prototype.has = originalSetHas;
      }

      expect(thrown).toBeUndefined();
      expect(parsed).toBeUndefined();
    }
  );

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

  it('accepts only the exact data-free APS top-mount notification', () => {
    const message = {
      message: 'TS APS Top Mount Started',
      version: 1,
      lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
    };
    const adapter = createBrowserMessagingAdapter(createTarget());
    expect(adapter.parseProtocolMessage('apsTopMountStarted', message)).toEqual(message);
    expect(
      adapter.parseProtocolMessage('apsTopMountStarted', {
        ...message,
        rendererUrl: 'https://publisher.example/integrations/aps/renderer/v2',
      })
    ).toBeUndefined();
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
      expectedRendererUrl: 'https://publisher.example/integrations/aps/renderer/v2',
      validateApsRenderer: validator,
    });
    const parsed = adapter.parseProtocolMessage('apsEnvelope', {
      version: 1,
      nonce: 'n1_abcdefghijklmnopqrstuv',
      publisherOrigin: 'https://publisher.example',
      renderer,
    });

    expect(validator).toHaveBeenCalledTimes(1);
    expect(canonical).not.toBe(renderer);
    expect(Object.getPrototypeOf(canonical)).toBeNull();
    expect(Object.isFrozen(canonical)).toBe(true);
    expect((canonical as Record<string, unknown>)['bidId']).toBe('bid-1');
    expect(parsed?.['renderer']).toBe(canonical);
  });

  it('rejects APS renderer accessors, proxies, and unknown keys before validation', () => {
    const validator = vi.fn(() => true);
    const adapter = createBrowserMessagingAdapter(createTarget(), {
      expectedPublisherOrigin: 'https://publisher.example',
      expectedRendererUrl: 'https://publisher.example/integrations/aps/renderer/v2',
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
        rendererVersion: '4',
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
      rendererVersion: '4',
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
    const single = createPort();
    const pairFirst = createPort();
    const pairSecond = createPort();

    const zero = adapter.extractTransferredPorts({ ports: [] }, 0);
    const one = adapter.extractTransferredPorts({ ports: [single] }, 1);
    const two = adapter.extractTransferredPorts({ ports: [pairFirst, pairSecond] }, 2);

    expect(zero).toEqual([]);
    expect(one).toHaveLength(1);
    expect(two).toHaveLength(2);
    expect(Object.isFrozen(zero)).toBe(true);
    expect(Object.isFrozen(one?.[0])).toBe(true);
    expect(one?.[0]).not.toHaveProperty('postMessage');
  });

  it('inspects every available refusal port without treating malformed counts as exact', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const first = createPort();
    const second = createPort();
    const third = createPort();
    const malformed = { close: vi.fn() };
    const laterUsable = createPort();

    const overflow = adapter.inspectTransferredPorts({ ports: [first, second, third] });
    expect(overflow).toMatchObject({ exactShape: true, originalCount: 3 });
    expect(overflow?.ports).toHaveLength(3);
    expect(Object.isFrozen(overflow)).toBe(true);
    expect(Object.isFrozen(overflow?.ports)).toBe(true);
    overflow?.ports.forEach((port) => port.close());
    expect(first.close).toHaveBeenCalledOnce();
    expect(second.close).toHaveBeenCalledOnce();
    expect(third.close).toHaveBeenCalledOnce();

    const mixed = adapter.inspectTransferredPorts({ ports: [malformed, laterUsable] });
    expect(mixed).toMatchObject({ exactShape: true, originalCount: 2 });
    expect(mixed?.ports).toHaveLength(1);
    expect(malformed.close).toHaveBeenCalledOnce();
    mixed?.ports[0]?.close();
    expect(laterUsable.close).toHaveBeenCalledOnce();
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

  it('bounds sparse hostile array inspection by present own keys rather than declared length', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const raw = createPort();
    const sparse = [raw];
    sparse.length = 0xffff_ffff;
    let descriptorReads = 0;
    const hostile = new Proxy(sparse, {
      getOwnPropertyDescriptor(target, key) {
        descriptorReads += 1;
        if (descriptorReads > 8) throw new Error('unbounded descriptor scan');
        return Reflect.getOwnPropertyDescriptor(target, key);
      },
    });

    expect(adapter.extractTransferredPorts({ ports: hostile }, 1)).toBeUndefined();
    expect(descriptorReads).toBeLessThanOrEqual(3);
    expect(raw.close).toHaveBeenCalledOnce();
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

  it('rolls back every attempted port listener when message, messageerror, or start fails', () => {
    for (const failure of ['message', 'messageerror', 'start'] as const) {
      const adapter = createBrowserMessagingAdapter(createTarget());
      const raw = createPort();
      raw.addEventListener.mockImplementation((type, listener) => {
        (type === 'messageerror' ? raw.messageErrorListeners : raw.listeners).add(listener);
        if (failure === type) {
          throw new Error(`${type} add failed`);
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
      if (failure !== 'message') {
        expect(raw.removeEventListener).toHaveBeenCalledWith('messageerror', expect.any(Function));
      }
      expect(raw.removeEventListener).toHaveBeenCalledTimes(failure === 'message' ? 1 : 2);
    }
  });

  it.each(['before', 'after'] as const)(
    'rolls back port listener ownership when its registry throws %s insertion',
    (failure) => {
      const adapter = createBrowserMessagingAdapter(createTarget());
      const raw = createPort();
      const [port] = adapter.extractTransferredPorts({ ports: [raw] }, 1) ?? [];
      if (!port) throw new Error('Expected one port');
      const messageListener = vi.fn();
      const messageErrorListener = vi.fn();
      const originalSetAdd = Set.prototype.add;
      const originalSetDelete = Set.prototype.delete;
      Set.prototype.add = function (this: Set<unknown>, value: unknown): Set<unknown> {
        if (failure === 'after') Reflect.apply(originalSetAdd, this, [value]);
        throw new Error(`listener registry failed ${failure} insertion`);
      } as typeof Set.prototype.add;
      Set.prototype.delete = function (): boolean {
        throw new Error('listener publication rollback delete failed');
      } as typeof Set.prototype.delete;

      let dispose: (() => void) | undefined;
      let thrown: unknown;
      try {
        dispose = port.listen(messageListener, messageErrorListener);
      } catch (error) {
        thrown = error;
      } finally {
        Set.prototype.add = originalSetAdd;
        Set.prototype.delete = originalSetDelete;
      }

      expect(thrown).toBeUndefined();
      expect(raw.listeners.size).toBe(0);
      expect(raw.messageErrorListeners.size).toBe(0);
      expect(() => dispose?.()).not.toThrow();
      port.close();
      expect(raw.removeEventListener).not.toHaveBeenCalled();
      expect(raw.close).toHaveBeenCalledTimes(1);
    }
  );

  it('removes port listeners when Set.delete is poisoned during unsubscribe and close', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const firstRaw = createPort();
    const secondRaw = createPort();
    const originalSetDelete = Set.prototype.delete;
    for (const raw of [firstRaw, secondRaw]) {
      raw.removeEventListener.mockImplementation((type, listener) => {
        const registered = type === 'messageerror' ? raw.messageErrorListeners : raw.listeners;
        Reflect.apply(originalSetDelete, registered, [listener]);
      });
    }
    const [first] = adapter.extractTransferredPorts({ ports: [firstRaw] }, 1) ?? [];
    const [second] = adapter.extractTransferredPorts({ ports: [secondRaw] }, 1) ?? [];
    if (!first || !second) throw new Error('Expected two ports');
    const unsubscribe = first.listen(vi.fn(), vi.fn());
    second.listen(vi.fn(), vi.fn());

    Set.prototype.delete = function (): boolean {
      throw new Error('port listener registry delete failed');
    } as typeof Set.prototype.delete;
    try {
      expect(() => unsubscribe()).not.toThrow();
      expect(() => unsubscribe()).not.toThrow();
      expect(() => second.close()).not.toThrow();
      expect(() => second.close()).not.toThrow();
    } finally {
      Set.prototype.delete = originalSetDelete;
    }

    expect(firstRaw.listeners.size).toBe(0);
    expect(firstRaw.messageErrorListeners.size).toBe(0);
    expect(secondRaw.listeners.size).toBe(0);
    expect(secondRaw.messageErrorListeners.size).toBe(0);
    expect(firstRaw.removeEventListener).toHaveBeenCalledTimes(2);
    expect(secondRaw.removeEventListener).toHaveBeenCalledTimes(2);
    expect(secondRaw.close).toHaveBeenCalledTimes(1);
    first.close();
  });

  it('rolls back both port listeners when setup and Set.delete fail together', () => {
    const adapter = createBrowserMessagingAdapter(createTarget());
    const raw = createPort();
    const originalSetDelete = Set.prototype.delete;
    raw.removeEventListener.mockImplementation((type, listener) => {
      const registered = type === 'messageerror' ? raw.messageErrorListeners : raw.listeners;
      Reflect.apply(originalSetDelete, registered, [listener]);
    });
    raw.addEventListener.mockImplementation((type, listener) => {
      (type === 'messageerror' ? raw.messageErrorListeners : raw.listeners).add(listener);
      if (type === 'messageerror') throw new Error('messageerror setup failed');
    });
    const [port] = adapter.extractTransferredPorts({ ports: [raw] }, 1) ?? [];
    if (!port) throw new Error('Expected one port');
    Set.prototype.delete = function (): boolean {
      throw new Error('setup rollback registry delete failed');
    } as typeof Set.prototype.delete;

    let unsubscribe: (() => void) | undefined;
    try {
      expect(() => {
        unsubscribe = port.listen(vi.fn(), vi.fn());
      }).not.toThrow();
      expect(() => unsubscribe?.()).not.toThrow();
    } finally {
      Set.prototype.delete = originalSetDelete;
    }

    expect(raw.listeners.size).toBe(0);
    expect(raw.messageErrorListeners.size).toBe(0);
    expect(raw.removeEventListener).toHaveBeenCalledTimes(2);
    expect(() => port.close()).not.toThrow();
    expect(raw.close).toHaveBeenCalledTimes(1);
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
    expect(dispose).toBeTypeOf('function');
    expect(() => installed[0]?.({} as MessageEvent)).not.toThrow();
    expect(() => dispose?.()).not.toThrow();

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
    expect(throwingTarget.installCaptureListener(vi.fn())).toBeUndefined();
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
    expect(dispose).toBeUndefined();
    expect(removeEventListener).toHaveBeenCalledTimes(1);
  });
});
