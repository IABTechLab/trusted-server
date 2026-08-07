import { describe, expect, it, vi } from 'vitest';

import { createBrowserMessagingAdapter } from '../../src/adapters/messaging';
import {
  createPucBridge,
  PUC_DYNAMIC_OWNER,
  type PucBridgeOptions,
  type PucRenderAttempt,
} from '../../src/services/puc_bridge';
import type { RenderFailureReason, RenderOutcome } from '../../src/services/render';
import type {
  ReservationClaimResult,
  ReservationRecognition,
  ReservationRenderSource,
} from '../../src/services/reservations';

const RESERVATION_ID = 'r1_abcdefghijklmnopqrstuv';
const LIFECYCLE_TICKET = 't1_abcdefghijklmnopqrstuv';

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

function exactOwnerRegistration(adId: string, lifecycleTicket = LIFECYCLE_TICKET): string {
  return JSON.stringify({
    message: 'TS Render Owner Register',
    adId,
    version: 1,
    lifecycleTicket,
  });
}

interface HarnessOptions {
  readonly claim?: PucBridgeOptions['reservations']['claim'];
  readonly messageChannel?: new () => { readonly port1: unknown; readonly port2: unknown };
  readonly mintLifecycleTicket?: PucBridgeOptions['mintLifecycleTicket'];
  readonly now?: PucBridgeOptions['now'];
  readonly publisherOrigin?: string;
  readonly rendererNonces?: PucBridgeOptions['rendererNonces'];
  readonly rendererUrl?: string;
  readonly resolveCacheAdm?: PucBridgeOptions['resolveCacheAdm'];
  readonly scheduler?: PucBridgeOptions['scheduler'];
}

function createHarness(
  recognize: (reservationId: unknown) => ReservationRecognition,
  options: HarnessOptions = {}
) {
  let listener: ((event: MessageEvent) => void) | undefined;
  const target = {
    addEventListener: vi.fn(
      (_type: 'message', next: (event: MessageEvent) => void, _capture: true) => {
        listener = next;
      }
    ),
    removeEventListener: vi.fn(),
    ...(options.messageChannel ? { MessageChannel: options.messageChannel } : {}),
  };
  const bridgeOptions: PucBridgeOptions = {
    messaging: createBrowserMessagingAdapter(target, {
      ...(options.publisherOrigin ? { expectedPublisherOrigin: options.publisherOrigin } : {}),
      ...(options.rendererUrl ? { expectedRendererUrl: options.rendererUrl } : {}),
      validateApsRenderer: () => true,
    }),
    reservations: {
      claim: options.claim ?? (() => ({ recognized: false }) satisfies ReservationClaimResult),
      recognize,
    },
    ...(options.mintLifecycleTicket ? { mintLifecycleTicket: options.mintLifecycleTicket } : {}),
    ...(options.now ? { now: options.now } : {}),
    ...(options.publisherOrigin ? { publisherOrigin: options.publisherOrigin } : {}),
    ...(options.rendererNonces ? { rendererNonces: options.rendererNonces } : {}),
    ...(options.rendererUrl ? { rendererUrl: options.rendererUrl } : {}),
    ...(options.resolveCacheAdm ? { resolveCacheAdm: options.resolveCacheAdm } : {}),
    ...(options.scheduler ? { scheduler: options.scheduler } : {}),
  };
  const bridge = createPucBridge(bridgeOptions);
  const dispatch = (event: Record<string, unknown>): void => {
    if (!listener) throw new Error('Expected the capture listener to be installed synchronously');
    listener(event as unknown as MessageEvent);
  };
  return { bridge, dispatch, target };
}

function createGamAttempt(kind: 'aps' | 'adm' | 'cache' = 'aps', index = 0) {
  const suffix = index.toString(36).padStart(22, '0').slice(-22);
  const id = `a1_${suffix}`;
  const reservationId = `r1_${suffix}`;
  const navigationGeneration = Object.freeze({ navigation: index });
  const generation = Object.freeze({ attempt: index });
  const winnerContext = Object.freeze({ selectedCpm: 1.25 });
  let state = 'created';
  let outcome: RenderOutcome | undefined;
  let renderSource: ReservationRenderSource | undefined;
  const settlementObservers: Array<(outcome: RenderOutcome) => void> = [];
  const owner = Object.freeze({
    id,
    slot: `slot-${index}`,
    navigationGeneration,
    generation,
    winnerContext,
    isCurrent: vi.fn(() => outcome === undefined),
    prepareWinnerContext: vi.fn(),
  });
  const artifact = Object.freeze({
    kind: 'puc' as const,
    attemptId: id,
    slot: owner.slot,
    navigationGeneration,
    dispose: vi.fn(),
  });
  const attempt = Object.freeze({
    id,
    slot: owner.slot,
    generation,
    navigationGeneration,
    get renderSource() {
      return renderSource;
    },
    beginGamClaim: vi.fn(() => {
      if (state !== 'created' || outcome !== undefined) return false;
      state = 'waiting_for_gam_and_claim';
      return true;
    }),
    admitClaimedWinner: vi.fn(() => {
      if (state !== 'waiting_for_gam_and_claim' || outcome !== undefined) return false;
      renderSource = Object.freeze(
        kind === 'aps'
          ? {
              type: 'aps',
              version: 1,
              accountId: 'publisher-account',
              bidId: 'bid-1',
              tagType: 'iframe',
              creativeUrl: 'https://creative.example/render',
              width: 300,
              height: 250,
              aaxResponse: 'renderer-envelope',
            }
          : kind === 'adm'
            ? {
                type: 'adm',
                version: 1,
                adm: '<main>fictional creative</main>',
                width: 300,
                height: 250,
              }
            : {
                type: 'cache',
                version: 1,
                cacheId: '12345678-1234-4123-8123-123456789012',
                fetchUrl:
                  'https://cache.example/pbc/v1/cache?uuid=12345678-1234-4123-8123-123456789012',
                width: 300,
                height: 250,
              }
      ) as ReservationRenderSource;
      return true;
    }),
    ownerClaimed: vi.fn(() => {
      if (!renderSource || state !== 'waiting_for_gam_and_claim' || outcome !== undefined) {
        return false;
      }
      state = 'waiting_for_owner';
      return true;
    }),
    ownerRegistered: vi.fn(() => {
      if (state !== 'waiting_for_owner' || outcome !== undefined) return false;
      state = 'waiting_for_insertion';
      return true;
    }),
    beginApsDocument: vi.fn(() => {
      if (state !== 'waiting_for_insertion' || outcome !== undefined) return false;
      state = 'waiting_for_document';
      return true;
    }),
    beginAdm: vi.fn(() => {
      if (state !== 'waiting_for_insertion' || outcome !== undefined) return false;
      state = 'waiting_for_adm';
      return true;
    }),
    apsDocumentAccepted: vi.fn(() => {
      if (state !== 'waiting_for_document' || outcome !== undefined) return false;
      state = 'waiting_for_aps_completion';
      return true;
    }),
    accept: vi.fn(() => {
      if (
        (state !== 'waiting_for_aps_completion' && state !== 'waiting_for_adm') ||
        outcome !== undefined
      ) {
        return false;
      }
      outcome = Object.freeze({ outcome: 'accepted' });
      state = 'accepted';
      for (const observer of settlementObservers) observer(outcome);
      return true;
    }),
    cancel: vi.fn((reason: 'caller_aborted' | 'superseded' | 'navigation_disposed') => {
      if (outcome !== undefined) return false;
      outcome = Object.freeze({ outcome: 'cancelled' as const, reason });
      state = 'cancelled';
      for (const observer of settlementObservers) observer(outcome);
      return true;
    }),
    fail: vi.fn((reason: RenderFailureReason) => {
      if (outcome !== undefined) return false;
      outcome = Object.freeze({ outcome: 'failed', reason });
      state = 'failed';
      for (const observer of settlementObservers) observer(outcome);
      return true;
    }),
    onSettled: vi.fn((callback: (terminal: RenderOutcome) => void) => {
      if (outcome !== undefined) return false;
      settlementObservers.push(callback);
      return true;
    }),
    snapshot: vi.fn(() => Object.freeze({ state, outcome, history: Object.freeze([state]) })),
  });
  return { artifact, attempt, owner, reservationId };
}

function dispatchPortMessage(
  port: ReturnType<typeof createPort>,
  data: unknown,
  ports: readonly unknown[] = []
): void {
  const listener = port.addEventListener.mock.calls.find((call) => call[0] === 'message')?.[1] as
    ((event: { data: unknown; ports: readonly unknown[] }) => void) | undefined;
  if (!listener) throw new Error('Expected the retained port listener to be installed');
  listener({ data, ports });
}

function createClock() {
  let now = 0;
  let nextHandle = 0;
  const tasks = new Map<number, { callback: () => void; deadline: number }>();
  const scheduler = {
    set: vi.fn((callback: () => void, milliseconds: number): number => {
      nextHandle += 1;
      tasks.set(nextHandle, { callback, deadline: now + milliseconds });
      return nextHandle;
    }),
    clear: vi.fn((handle: unknown): void => {
      if (typeof handle === 'number') tasks.delete(handle);
    }),
  };
  const advance = (milliseconds: number): void => {
    now += milliseconds;
    for (const [handle, task] of [...tasks]) {
      if (task.deadline <= now) {
        tasks.delete(handle);
        task.callback();
      }
    }
  };
  return { advance, now: () => now, scheduler };
}

function issueReadyTicket(
  harness: ReturnType<typeof createHarness>,
  gam: ReturnType<typeof createGamAttempt>,
  source: object
): void {
  expect(
    harness.bridge.registerGamAttempt({
      artifact: gam.artifact,
      attempt: gam.attempt,
      owner: gam.owner,
      reservationId: gam.reservationId,
    })
  ).toBe(true);
  harness.dispatch({
    data: exactRequest(gam.reservationId),
    ports: [createPort()],
    source,
    stopImmediatePropagation: vi.fn(),
  });
  expect(
    harness.bridge.recordNonemptyGam({
      artifact: gam.artifact,
      attempt: gam.attempt,
      owner: gam.owner,
      reservationId: gam.reservationId,
    })
  ).toBe(true);
  expect(harness.bridge.snapshotInventoryForTest().liveTickets).toBe(1);
}

describe('Universal Creative bridge dispatcher', () => {
  it('installs owner iframe lifecycle handlers before assigning either document source', () => {
    const admStart = PUC_DYNAMIC_OWNER.indexOf('const insertAdm');
    const apsStart = PUC_DYNAMIC_OWNER.indexOf('const insertAps');
    const controlStart = PUC_DYNAMIC_OWNER.indexOf('const receiveControl');
    const admOwner = PUC_DYNAMIC_OWNER.slice(admStart, apsStart);
    const apsOwner = PUC_DYNAMIC_OWNER.slice(apsStart, controlStart);

    expect(new TextEncoder().encode(PUC_DYNAMIC_OWNER).byteLength).toBeLessThanOrEqual(64 * 1_024);
    expect(admStart).toBeGreaterThanOrEqual(0);
    expect(apsStart).toBeGreaterThan(admStart);
    expect(controlStart).toBeGreaterThan(apsStart);
    expect(admOwner.indexOf('next.onload =')).toBeLessThan(admOwner.indexOf('next.srcdoc ='));
    expect(admOwner.indexOf('next.onerror =')).toBeLessThan(admOwner.indexOf('next.srcdoc ='));
    expect(apsOwner.indexOf('next.onload =')).toBeLessThan(apsOwner.indexOf('next.src ='));
    expect(apsOwner.indexOf('next.onerror =')).toBeLessThan(apsOwner.indexOf('next.src ='));
  });

  it('runs the checked-in PUC owner through helper registration and final ADM settlement', async () => {
    const dynamicWindow = window as unknown as {
      render?: (
        data: Readonly<Record<string, unknown>>,
        helper: Readonly<Record<string, unknown>>,
        ownerWindow: Window
      ) => Promise<void>;
    };
    window.eval(PUC_DYNAMIC_OWNER);
    const stopListening = vi.fn();
    let registrationCallback: ((event: unknown) => void) | undefined;
    const sendMessage = vi.fn(
      (
        type: string,
        payload: Readonly<Record<string, unknown>>,
        callback: (event: unknown) => void
      ) => {
        registrationCallback = callback;
        return stopListening;
      }
    );
    let controlListener: ((event: unknown) => void) | undefined;
    const controlPort = {
      close: vi.fn(),
      postMessage: vi.fn(),
      start: vi.fn(),
      set onmessage(listener: ((event: unknown) => void) | null) {
        controlListener = listener ?? undefined;
      },
      set onmessageerror(_listener: ((event: unknown) => void) | null) {},
    };

    try {
      const ownerData = window.JSON.parse(
        JSON.stringify({
          adId: RESERVATION_ID,
          message: 'Prebid Response',
          renderer: PUC_DYNAMIC_OWNER,
          rendererVersion: '3',
          tsOwner: {
            version: 1,
            status: 'ready',
            kind: 'adm',
            lifecycleTicket: LIFECYCLE_TICKET,
          },
        })
      ) as Readonly<Record<string, unknown>>;
      const rendered = dynamicWindow.render!(ownerData, { sendMessage }, window);
      expect(sendMessage).toHaveBeenCalledWith(
        'TS Render Owner Register',
        { version: 1, lifecycleTicket: LIFECYCLE_TICKET },
        expect.any(Function)
      );
      registrationCallback?.({
        data: JSON.stringify({
          message: 'TS Render Owner Registered',
          adId: RESERVATION_ID,
          version: 1,
          lifecycleTicket: LIFECYCLE_TICKET,
        }),
        ports: [controlPort],
      });
      expect(stopListening).toHaveBeenCalledOnce();
      expect(controlPort.start).toHaveBeenCalledOnce();

      controlListener?.({
        data: {
          message: 'TS ADM Start',
          version: 1,
          lifecycleTicket: LIFECYCLE_TICKET,
          source: {
            type: 'adm',
            version: 1,
            adm: '<main>remote creative</main>',
            width: 300,
            height: 250,
          },
        },
        ports: [],
      });
      const frame = document.body.querySelector<HTMLIFrameElement>('iframe');
      expect(frame).not.toBeNull();
      expect(frame?.srcdoc).toContain('<main>remote creative</main>');
      expect(frame?.getAttribute('sandbox')).toBe(
        'allow-forms allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation'
      );
      expect(controlPort.postMessage).toHaveBeenCalledWith({
        message: 'TS Owner Inserted',
        version: 1,
        lifecycleTicket: LIFECYCLE_TICKET,
      });

      frame?.dispatchEvent(new Event('load'));
      expect(controlPort.postMessage).toHaveBeenCalledWith({
        message: 'TS ADM Loaded',
        version: 1,
        lifecycleTicket: LIFECYCLE_TICKET,
      });
      controlListener?.({
        data: {
          message: 'TS Owner Settled',
          version: 1,
          lifecycleTicket: LIFECYCLE_TICKET,
          outcome: 'accepted',
        },
        ports: [],
      });

      await expect(rendered).resolves.toBeUndefined();
      expect(frame?.isConnected).toBe(true);
      expect(controlPort.close).toHaveBeenCalledOnce();
    } finally {
      delete dynamicWindow.render;
      document.body.innerHTML = '';
    }
  });

  it('accepts the optional APS creative id and preserves no-referrer on the owner iframe', async () => {
    const dynamicWindow = window as unknown as {
      render?: (
        data: Readonly<Record<string, unknown>>,
        helper: Readonly<Record<string, unknown>>,
        ownerWindow: Window
      ) => Promise<void>;
    };
    window.eval(PUC_DYNAMIC_OWNER);
    let registrationCallback: ((event: unknown) => void) | undefined;
    const sendMessage = vi.fn(
      (
        _type: string,
        _payload: Readonly<Record<string, unknown>>,
        callback: (event: unknown) => void
      ) => {
        registrationCallback = callback;
        return vi.fn();
      }
    );
    let controlListener: ((event: unknown) => void) | undefined;
    const controlPort = {
      close: vi.fn(),
      postMessage: vi.fn(),
      start: vi.fn(),
      set onmessage(listener: ((event: unknown) => void) | null) {
        controlListener = listener ?? undefined;
      },
      set onmessageerror(_listener: ((event: unknown) => void) | null) {},
    };
    const documentPort = createPort();

    try {
      const rendered = dynamicWindow.render!(
        window.JSON.parse(
          JSON.stringify({
            adId: RESERVATION_ID,
            message: 'Prebid Response',
            renderer: PUC_DYNAMIC_OWNER,
            rendererVersion: '3',
            tsOwner: {
              version: 1,
              status: 'ready',
              kind: 'aps',
              lifecycleTicket: LIFECYCLE_TICKET,
            },
          })
        ) as Readonly<Record<string, unknown>>,
        { sendMessage },
        window
      );
      registrationCallback?.({
        data: JSON.stringify({
          message: 'TS Render Owner Registered',
          adId: RESERVATION_ID,
          version: 1,
          lifecycleTicket: LIFECYCLE_TICKET,
        }),
        ports: [controlPort],
      });
      controlListener?.({
        data: {
          message: 'TS APS Start',
          version: 1,
          lifecycleTicket: LIFECYCLE_TICKET,
          rendererUrl: 'https://publisher.example/integrations/aps/renderer/v1',
          envelope: {
            version: 1,
            nonce: 'n1_abcdefghijklmnopqrstuv',
            publisherOrigin: 'https://publisher.example',
            renderer: {
              type: 'aps',
              version: 1,
              accountId: 'publisher-account',
              bidId: 'bid-1',
              tagType: 'iframe',
              creativeUrl: 'https://creative.example/render',
              width: 300,
              height: 250,
              aaxResponse: 'renderer-envelope',
              creativeId: 'creative-1',
            },
          },
        },
        ports: [documentPort],
      });

      const frame = document.body.querySelector<HTMLIFrameElement>('iframe');
      expect(frame).not.toBeNull();
      expect(frame?.getAttribute('referrerpolicy')).toBe('no-referrer');
      controlListener?.({
        data: {
          message: 'TS Owner Settled',
          version: 1,
          lifecycleTicket: LIFECYCLE_TICKET,
          outcome: 'accepted',
        },
        ports: [],
      });
      await expect(rendered).resolves.toBeUndefined();
    } finally {
      delete dynamicWindow.render;
      document.body.innerHTML = '';
    }
  });

  it('keeps APS ownership alive after a local frame error until the kernel settles failure', async () => {
    vi.useFakeTimers();
    const dynamicWindow = window as unknown as {
      render?: (
        data: Readonly<Record<string, unknown>>,
        helper: Readonly<Record<string, unknown>>,
        ownerWindow: Window
      ) => Promise<void>;
    };
    window.eval(PUC_DYNAMIC_OWNER);
    let registrationCallback: ((event: unknown) => void) | undefined;
    const sendMessage = vi.fn(
      (
        _type: string,
        _payload: Readonly<Record<string, unknown>>,
        callback: (event: unknown) => void
      ) => {
        registrationCallback = callback;
        return vi.fn();
      }
    );
    let controlListener: ((event: unknown) => void) | undefined;
    const controlPort = {
      close: vi.fn(),
      postMessage: vi.fn(),
      start: vi.fn(),
      set onmessage(listener: ((event: unknown) => void) | null) {
        controlListener = listener ?? undefined;
      },
      set onmessageerror(_listener: ((event: unknown) => void) | null) {},
    };
    const documentPort = createPort();
    let rendered: Promise<void> | undefined;

    try {
      rendered = dynamicWindow.render!(
        window.JSON.parse(
          JSON.stringify({
            adId: RESERVATION_ID,
            message: 'Prebid Response',
            renderer: PUC_DYNAMIC_OWNER,
            rendererVersion: '3',
            tsOwner: {
              version: 1,
              status: 'ready',
              kind: 'aps',
              lifecycleTicket: LIFECYCLE_TICKET,
            },
          })
        ) as Readonly<Record<string, unknown>>,
        { sendMessage },
        window
      );
      registrationCallback?.({
        data: JSON.stringify({
          message: 'TS Render Owner Registered',
          adId: RESERVATION_ID,
          version: 1,
          lifecycleTicket: LIFECYCLE_TICKET,
        }),
        ports: [controlPort],
      });
      controlListener?.({
        data: {
          message: 'TS APS Start',
          version: 1,
          lifecycleTicket: LIFECYCLE_TICKET,
          rendererUrl: 'https://publisher.example/integrations/aps/renderer/v1',
          envelope: {
            version: 1,
            nonce: 'n1_abcdefghijklmnopqrstuv',
            publisherOrigin: 'https://publisher.example',
            renderer: {
              type: 'aps',
              version: 1,
              accountId: 'publisher-account',
              bidId: 'bid-1',
              tagType: 'iframe',
              creativeUrl: 'https://creative.example/render',
              width: 300,
              height: 250,
              aaxResponse: 'renderer-envelope',
            },
          },
        },
        ports: [documentPort],
      });

      const frame = document.body.querySelector<HTMLIFrameElement>('iframe');
      expect(frame).not.toBeNull();
      frame?.dispatchEvent(new Event('error'));
      const immediate = rendered.then(
        () => 'resolved',
        () => 'rejected'
      );
      await Promise.resolve();
      expect(await Promise.race([immediate, Promise.resolve('pending')])).toBe('pending');
      expect(document.body.querySelector('iframe')).toBeNull();
      expect(documentPort.close).toHaveBeenCalledOnce();
      expect(controlPort.close).not.toHaveBeenCalled();

      controlListener?.({
        data: {
          message: 'TS Owner Settled',
          version: 1,
          lifecycleTicket: LIFECYCLE_TICKET,
          outcome: 'failed',
          reason: 'runner_no_load',
        },
        ports: [],
      });
      await expect(rendered).rejects.toThrow('runner_no_load');
      expect(controlPort.close).toHaveBeenCalledOnce();
    } finally {
      await vi.runAllTimersAsync();
      await rendered?.catch(() => undefined);
      vi.useRealTimers();
      delete dynamicWindow.render;
      document.body.innerHTML = '';
    }
  });

  it('fails closed immediately when the PUC helper does not return its disposer', async () => {
    vi.useFakeTimers();
    const dynamicWindow = window as unknown as {
      render?: (
        data: Readonly<Record<string, unknown>>,
        helper: Readonly<Record<string, unknown>>,
        ownerWindow: Window
      ) => Promise<void>;
    };
    window.eval(PUC_DYNAMIC_OWNER);
    let rendered: Promise<void> | undefined;

    try {
      rendered = dynamicWindow.render!(
        window.JSON.parse(
          JSON.stringify({
            adId: RESERVATION_ID,
            message: 'Prebid Response',
            renderer: PUC_DYNAMIC_OWNER,
            rendererVersion: '3',
            tsOwner: {
              version: 1,
              status: 'ready',
              kind: 'adm',
              lifecycleTicket: LIFECYCLE_TICKET,
            },
          })
        ) as Readonly<Record<string, unknown>>,
        { sendMessage: vi.fn(() => undefined) },
        window
      );
      const immediate = rendered.then(
        () => 'resolved',
        () => 'rejected'
      );

      await Promise.resolve();
      expect(await Promise.race([immediate, Promise.resolve('pending')])).toBe('rejected');
    } finally {
      await vi.runAllTimersAsync();
      await rendered?.catch(() => undefined);
      vi.useRealTimers();
      delete dynamicWindow.render;
      document.body.innerHTML = '';
    }
  });

  it('rejects registration at exactly three seconds, disposes the helper, and closes a late port', async () => {
    vi.useFakeTimers();
    const dynamicWindow = window as unknown as {
      render?: (
        data: Readonly<Record<string, unknown>>,
        helper: Readonly<Record<string, unknown>>,
        ownerWindow: Window
      ) => Promise<void>;
    };
    window.eval(PUC_DYNAMIC_OWNER);
    const stopListening = vi.fn();
    let registrationCallback: ((event: unknown) => void) | undefined;
    const sendMessage = vi.fn(
      (
        _type: string,
        _payload: Readonly<Record<string, unknown>>,
        callback: (event: unknown) => void
      ) => {
        registrationCallback = callback;
        return stopListening;
      }
    );
    let rendered: Promise<void> | undefined;
    let settlement = 'pending';

    try {
      rendered = dynamicWindow.render!(
        window.JSON.parse(
          JSON.stringify({
            adId: RESERVATION_ID,
            message: 'Prebid Response',
            renderer: PUC_DYNAMIC_OWNER,
            rendererVersion: '3',
            tsOwner: {
              version: 1,
              status: 'ready',
              kind: 'adm',
              lifecycleTicket: LIFECYCLE_TICKET,
            },
          })
        ) as Readonly<Record<string, unknown>>,
        { sendMessage },
        window
      );
      void rendered.then(
        () => {
          settlement = 'resolved';
        },
        () => {
          settlement = 'rejected';
        }
      );

      await vi.advanceTimersByTimeAsync(2_999);
      expect(settlement).toBe('pending');
      expect(stopListening).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(1);
      expect(settlement).toBe('rejected');
      expect(stopListening).toHaveBeenCalledOnce();

      const latePort = createPort();
      registrationCallback?.({ data: '{}', ports: [latePort] });
      expect(latePort.close).toHaveBeenCalledOnce();
      expect(stopListening).toHaveBeenCalledOnce();
    } finally {
      await rendered?.catch(() => undefined);
      vi.useRealTimers();
      delete dynamicWindow.render;
      document.body.innerHTML = '';
    }
  });

  it('removes uncommitted owner DOM at the exact twenty-second watchdog boundary', async () => {
    vi.useFakeTimers();
    const dynamicWindow = window as unknown as {
      render?: (
        data: Readonly<Record<string, unknown>>,
        helper: Readonly<Record<string, unknown>>,
        ownerWindow: Window
      ) => Promise<void>;
    };
    window.eval(PUC_DYNAMIC_OWNER);
    let registrationCallback: ((event: unknown) => void) | undefined;
    const sendMessage = vi.fn(
      (
        _type: string,
        _payload: Readonly<Record<string, unknown>>,
        callback: (event: unknown) => void
      ) => {
        registrationCallback = callback;
        return vi.fn();
      }
    );
    let controlListener: ((event: unknown) => void) | undefined;
    const controlPort = {
      close: vi.fn(),
      postMessage: vi.fn(),
      start: vi.fn(),
      set onmessage(listener: ((event: unknown) => void) | null) {
        controlListener = listener ?? undefined;
      },
      set onmessageerror(_listener: ((event: unknown) => void) | null) {},
    };
    let rendered: Promise<void> | undefined;
    let settlement = 'pending';

    try {
      rendered = dynamicWindow.render!(
        window.JSON.parse(
          JSON.stringify({
            adId: RESERVATION_ID,
            message: 'Prebid Response',
            renderer: PUC_DYNAMIC_OWNER,
            rendererVersion: '3',
            tsOwner: {
              version: 1,
              status: 'ready',
              kind: 'adm',
              lifecycleTicket: LIFECYCLE_TICKET,
            },
          })
        ) as Readonly<Record<string, unknown>>,
        { sendMessage },
        window
      );
      void rendered.then(
        () => {
          settlement = 'resolved';
        },
        () => {
          settlement = 'rejected';
        }
      );
      registrationCallback?.({
        data: JSON.stringify({
          message: 'TS Render Owner Registered',
          adId: RESERVATION_ID,
          version: 1,
          lifecycleTicket: LIFECYCLE_TICKET,
        }),
        ports: [controlPort],
      });
      controlListener?.({
        data: {
          message: 'TS ADM Start',
          version: 1,
          lifecycleTicket: LIFECYCLE_TICKET,
          source: {
            type: 'adm',
            version: 1,
            adm: '<main>uncommitted creative</main>',
            width: 300,
            height: 250,
          },
        },
        ports: [],
      });
      const frame = document.body.querySelector<HTMLIFrameElement>('iframe');
      expect(frame?.isConnected).toBe(true);

      await vi.advanceTimersByTimeAsync(19_999);
      expect(settlement).toBe('pending');
      expect(frame?.isConnected).toBe(true);
      expect(controlPort.close).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(1);
      expect(settlement).toBe('rejected');
      expect(frame?.isConnected).toBe(false);
      expect(controlPort.close).toHaveBeenCalledOnce();
      await vi.advanceTimersByTimeAsync(1);
      expect(controlPort.close).toHaveBeenCalledOnce();
    } finally {
      await rendered?.catch(() => undefined);
      vi.useRealTimers();
      delete dynamicWindow.render;
      document.body.innerHTML = '';
    }
  });

  it('refuses an APS owner start whose renderer URL is outside the publisher origin', async () => {
    vi.useFakeTimers();
    const dynamicWindow = window as unknown as {
      render?: (
        data: Readonly<Record<string, unknown>>,
        helper: Readonly<Record<string, unknown>>,
        ownerWindow: Window
      ) => Promise<void>;
    };
    window.eval(PUC_DYNAMIC_OWNER);
    let registrationCallback: ((event: unknown) => void) | undefined;
    const sendMessage = vi.fn(
      (
        _type: string,
        _payload: Readonly<Record<string, unknown>>,
        callback: (event: unknown) => void
      ) => {
        registrationCallback = callback;
        return vi.fn();
      }
    );
    let controlListener: ((event: unknown) => void) | undefined;
    const controlPort = {
      close: vi.fn(),
      postMessage: vi.fn(),
      start: vi.fn(),
      set onmessage(listener: ((event: unknown) => void) | null) {
        controlListener = listener ?? undefined;
      },
      set onmessageerror(_listener: ((event: unknown) => void) | null) {},
    };
    const documentPort = createPort();
    let rendered: Promise<void> | undefined;

    try {
      rendered = dynamicWindow.render!(
        window.JSON.parse(
          JSON.stringify({
            adId: RESERVATION_ID,
            message: 'Prebid Response',
            renderer: PUC_DYNAMIC_OWNER,
            rendererVersion: '3',
            tsOwner: {
              version: 1,
              status: 'ready',
              kind: 'aps',
              lifecycleTicket: LIFECYCLE_TICKET,
            },
          })
        ) as Readonly<Record<string, unknown>>,
        { sendMessage },
        window
      );
      registrationCallback?.({
        data: JSON.stringify({
          message: 'TS Render Owner Registered',
          adId: RESERVATION_ID,
          version: 1,
          lifecycleTicket: LIFECYCLE_TICKET,
        }),
        ports: [controlPort],
      });
      controlListener?.({
        data: {
          message: 'TS APS Start',
          version: 1,
          lifecycleTicket: LIFECYCLE_TICKET,
          rendererUrl: 'https://attacker.example/integrations/aps/renderer/v1',
          envelope: {
            version: 1,
            nonce: 'n1_abcdefghijklmnopqrstuv',
            publisherOrigin: 'https://publisher.example',
            renderer: {
              type: 'aps',
              version: 1,
              accountId: 'publisher-account',
              bidId: 'bid-1',
              tagType: 'iframe',
              creativeUrl: 'https://creative.example/render',
              width: 300,
              height: 250,
              aaxResponse: 'renderer-envelope',
            },
          },
        },
        ports: [documentPort],
      });
      const immediate = rendered.then(
        () => 'resolved',
        () => 'rejected'
      );

      await Promise.resolve();
      expect(await Promise.race([immediate, Promise.resolve('pending')])).toBe('rejected');
      expect(document.body.querySelector('iframe')).toBeNull();
      expect(documentPort.close).toHaveBeenCalledOnce();
    } finally {
      await vi.runAllTimersAsync();
      await rendered?.catch(() => undefined);
      vi.useRealTimers();
      delete dynamicWindow.render;
      document.body.innerHTML = '';
    }
  });

  it('installs one capture listener synchronously and removes only that listener on disposal', () => {
    const harness = createHarness(() => ({ recognized: false }));

    expect(harness.target.addEventListener).toHaveBeenCalledOnce();
    expect(harness.target.addEventListener.mock.calls[0]?.[0]).toBe('message');
    expect(harness.target.addEventListener.mock.calls[0]?.[2]).toBe(true);
    expect(harness.bridge.snapshotInventoryForTest()).toEqual({
      attempts: 0,
      disposed: false,
      liveTickets: 0,
      pendingClaims: 0,
      ticketTombstones: 0,
    });

    harness.bridge.dispose();
    harness.bridge.dispose();
    expect(harness.target.removeEventListener).toHaveBeenCalledOnce();
    expect(harness.target.removeEventListener.mock.calls[0]?.[0]).toBe('message');
    expect(harness.target.removeEventListener.mock.calls[0]?.[2]).toBe(true);
    expect(harness.bridge.snapshotInventoryForTest()).toEqual({
      attempts: 0,
      disposed: true,
      liveTickets: 0,
      pendingClaims: 0,
      ticketTombstones: 0,
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

  it('suppresses recognized requests with the wrong port count, refuses on the first, and closes every port', () => {
    const harness = createHarness(() => ({
      recognized: true,
      state: 'renderable',
      expiresAt: 1_000,
    }));
    const first = createPort();
    const second = createPort();
    const third = createPort();
    const stopImmediatePropagation = vi.fn();

    harness.dispatch({
      data: exactRequest(),
      ports: [first, second, third],
      source: Object.freeze({}),
      stopImmediatePropagation,
    });

    expect(stopImmediatePropagation).toHaveBeenCalledOnce();
    expect(first.postMessage).toHaveBeenCalledOnce();
    expect(JSON.parse(String(first.postMessage.mock.calls[0]?.[0]))).toEqual({
      message: 'Prebid Response',
      adId: RESERVATION_ID,
      rendererVersion: '3',
      tsOwner: { version: 1, status: 'refused' },
    });
    expect(first.postMessage.mock.calls[0]?.[1]).toEqual([]);
    expect(second.postMessage).not.toHaveBeenCalled();
    expect(first.close).toHaveBeenCalledOnce();
    expect(second.close).toHaveBeenCalledOnce();
    expect(third.close).toHaveBeenCalledOnce();

    const malformed = { close: vi.fn() };
    const laterUsable = createPort();
    harness.dispatch({
      data: exactRequest(),
      ports: [malformed, laterUsable],
      source: Object.freeze({}),
      stopImmediatePropagation: vi.fn(),
    });
    expect(malformed.close).toHaveBeenCalledOnce();
    expect(laterUsable.postMessage).toHaveBeenCalledOnce();
    expect(laterUsable.close).toHaveBeenCalledOnce();
    expect(harness.bridge.snapshotInventoryForTest().pendingClaims).toBe(0);
  });

  it('buffers only the first exact live claim and generically refuses a duplicate', () => {
    const gam = createGamAttempt('aps');
    const harness = createHarness(() => ({
      recognized: true,
      state: 'renderable',
      expiresAt: 1_000,
    }));
    const first = createPort();
    const duplicate = createPort();
    const source = Object.freeze({ frame: 'authoritative' });
    expect(
      harness.bridge.registerGamAttempt({
        artifact: gam.artifact,
        attempt: gam.attempt,
        owner: gam.owner,
        reservationId: RESERVATION_ID,
      })
    ).toBe(true);

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

  it('joins an early claim with nonempty GAM and exposes only owner kind and ticket', () => {
    const gam = createGamAttempt('aps');
    const source = Object.freeze({ frame: 'authoritative' });
    const claim = vi.fn(({ pucSource }: { pucSource: unknown }): ReservationClaimResult => ({
      recognized: true,
      claimed: true,
      pucSource: pucSource as object,
      expiresAt: 10_000,
    }));
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      {
        claim,
        mintLifecycleTicket: () => Object.freeze({ ok: true, value: LIFECYCLE_TICKET }),
      }
    );
    const port = createPort();
    expect(
      harness.bridge.registerGamAttempt({
        artifact: gam.artifact,
        attempt: gam.attempt,
        owner: gam.owner,
        reservationId: RESERVATION_ID,
      })
    ).toBe(true);

    harness.dispatch({
      data: exactRequest(),
      ports: [port],
      source,
      stopImmediatePropagation: vi.fn(),
    });
    expect(claim).not.toHaveBeenCalled();
    expect(port.postMessage).not.toHaveBeenCalled();

    expect(
      harness.bridge.recordNonemptyGam({
        artifact: gam.artifact,
        attempt: gam.attempt,
        owner: gam.owner,
        reservationId: RESERVATION_ID,
      })
    ).toBe(true);

    expect(claim).toHaveBeenCalledWith({
      attempt: gam.owner,
      navigationGeneration: gam.owner.navigationGeneration,
      pucSource: source,
      reservationId: RESERVATION_ID,
      slot: gam.owner.slot,
    });
    expect(gam.attempt.admitClaimedWinner).toHaveBeenCalledOnce();
    expect(gam.attempt.ownerClaimed).toHaveBeenCalledOnce();
    expect(port.postMessage).toHaveBeenCalledOnce();
    const response = JSON.parse(String(port.postMessage.mock.calls[0]?.[0]));
    expect(
      new TextEncoder().encode(String(port.postMessage.mock.calls[0]?.[0])).byteLength
    ).toBeLessThanOrEqual(72 * 1_024);
    expect(response).toEqual({
      message: 'Prebid Response',
      adId: RESERVATION_ID,
      renderer: PUC_DYNAMIC_OWNER,
      rendererVersion: '3',
      tsOwner: {
        version: 1,
        status: 'ready',
        kind: 'aps',
        lifecycleTicket: LIFECYCLE_TICKET,
      },
    });
    expect(response).not.toHaveProperty('source');
    expect(response).not.toHaveProperty('renderSource');
    expect(response).not.toHaveProperty('winnerContext');
    expect(port.postMessage.mock.calls[0]?.[1]).toEqual([]);
    expect(port.close).toHaveBeenCalledOnce();
    expect(harness.bridge.snapshotInventoryForTest()).toEqual({
      attempts: 1,
      disposed: false,
      liveTickets: 1,
      pendingClaims: 0,
      ticketTombstones: 0,
    });

    expect(gam.attempt.fail('internal_error')).toBe(true);
    expect(harness.bridge.snapshotInventoryForTest()).toEqual({
      attempts: 0,
      disposed: false,
      liveTickets: 0,
      pendingClaims: 0,
      ticketTombstones: 1,
    });
  });

  it('starts the exact three-second claim deadline only after nonempty GAM', () => {
    const clock = createClock();
    const gam = createGamAttempt('adm');
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      { now: clock.now, scheduler: clock.scheduler }
    );
    expect(
      harness.bridge.registerGamAttempt({
        artifact: gam.artifact,
        attempt: gam.attempt,
        owner: gam.owner,
        reservationId: RESERVATION_ID,
      })
    ).toBe(true);
    expect(
      harness.bridge.recordNonemptyGam({
        artifact: gam.artifact,
        attempt: gam.attempt,
        owner: gam.owner,
        reservationId: RESERVATION_ID,
      })
    ).toBe(true);

    expect(clock.scheduler.set).toHaveBeenCalledWith(expect.any(Function), 3_000);
    clock.advance(2_999);
    expect(gam.attempt.fail).not.toHaveBeenCalled();
    clock.advance(1);
    expect(gam.attempt.fail).toHaveBeenCalledWith('bridge_claim_timeout');
    expect(gam.artifact.dispose).toHaveBeenCalledOnce();
    expect(harness.bridge.snapshotInventoryForTest().attempts).toBe(0);
  });

  it('clears a GAM-first claim deadline when the exact request completes the join', () => {
    const clock = createClock();
    const gam = createGamAttempt('cache');
    const claim = vi.fn(({ pucSource }: { pucSource: unknown }): ReservationClaimResult => ({
      recognized: true,
      claimed: true,
      pucSource: pucSource as object,
      expiresAt: 10_000,
    }));
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      {
        claim,
        mintLifecycleTicket: () => Object.freeze({ ok: true, value: LIFECYCLE_TICKET }),
        now: clock.now,
        scheduler: clock.scheduler,
      }
    );
    expect(
      harness.bridge.registerGamAttempt({
        artifact: gam.artifact,
        attempt: gam.attempt,
        owner: gam.owner,
        reservationId: RESERVATION_ID,
      })
    ).toBe(true);
    expect(
      harness.bridge.recordNonemptyGam({
        artifact: gam.artifact,
        attempt: gam.attempt,
        owner: gam.owner,
        reservationId: RESERVATION_ID,
      })
    ).toBe(true);
    const port = createPort();
    harness.dispatch({
      data: exactRequest(),
      ports: [port],
      source: Object.freeze({}),
      stopImmediatePropagation: vi.fn(),
    });

    expect(port.postMessage).toHaveBeenCalledOnce();
    expect(gam.attempt.renderSource).toMatchObject({ type: 'cache', version: 1 });
    expect(JSON.parse(String(port.postMessage.mock.calls[0]?.[0])).tsOwner.kind).toBe('adm');
    expect(clock.scheduler.clear).toHaveBeenCalledOnce();
    clock.advance(2_999);
    expect(gam.attempt.fail).not.toHaveBeenCalled();
    clock.advance(1);
    expect(gam.attempt.fail).toHaveBeenCalledWith('owner_registration_timeout');
    expect(gam.attempt.fail).not.toHaveBeenCalledWith('bridge_claim_timeout');
  });

  it('checks all eight ticket draws against live and tombstoned entries', () => {
    const first = createGamAttempt('aps', 1);
    const second = createGamAttempt('aps', 2);
    let draws = 0;
    const claim = vi.fn(({ pucSource }: { pucSource: unknown }): ReservationClaimResult => ({
      recognized: true,
      claimed: true,
      pucSource: pucSource as object,
      expiresAt: 10_000,
    }));
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      {
        claim,
        mintLifecycleTicket: () => {
          draws += 1;
          return Object.freeze({ ok: true as const, value: LIFECYCLE_TICKET });
        },
      }
    );
    for (const gam of [first, second]) {
      expect(
        harness.bridge.registerGamAttempt({
          artifact: gam.artifact,
          attempt: gam.attempt,
          owner: gam.owner,
          reservationId: gam.reservationId,
        })
      ).toBe(true);
      const port = createPort();
      harness.dispatch({
        data: exactRequest(gam.reservationId),
        ports: [port],
        source: Object.freeze({ index: draws }),
        stopImmediatePropagation: vi.fn(),
      });
      expect(
        harness.bridge.recordNonemptyGam({
          artifact: gam.artifact,
          attempt: gam.attempt,
          owner: gam.owner,
          reservationId: gam.reservationId,
        })
      ).toBe(true);
      if (gam === first) {
        expect(JSON.parse(String(port.postMessage.mock.calls[0]?.[0])).tsOwner.status).toBe(
          'ready'
        );
        expect(gam.attempt.fail('internal_error')).toBe(true);
      } else {
        expect(JSON.parse(String(port.postMessage.mock.calls[0]?.[0])).tsOwner.status).toBe(
          'refused'
        );
        expect(gam.attempt.fail).toHaveBeenCalledWith('identity_generation_failed');
      }
    }
    expect(draws).toBe(9);
    expect(harness.bridge.snapshotInventoryForTest().ticketTombstones).toBe(1);
  });

  it('retains ticket tombstones through 2,999 ms and prunes them at 3,000 ms', () => {
    const clock = createClock();
    const gam = createGamAttempt('aps', 7);
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      {
        claim: ({ pucSource }) => ({
          recognized: true,
          claimed: true,
          pucSource: pucSource as object,
          expiresAt: 10_000,
        }),
        mintLifecycleTicket: () => Object.freeze({ ok: true, value: LIFECYCLE_TICKET }),
        now: clock.now,
        scheduler: clock.scheduler,
      }
    );
    expect(
      harness.bridge.registerGamAttempt({
        artifact: gam.artifact,
        attempt: gam.attempt,
        owner: gam.owner,
        reservationId: gam.reservationId,
      })
    ).toBe(true);
    harness.dispatch({
      data: exactRequest(gam.reservationId),
      ports: [createPort()],
      source: Object.freeze({}),
      stopImmediatePropagation: vi.fn(),
    });
    expect(
      harness.bridge.recordNonemptyGam({
        artifact: gam.artifact,
        attempt: gam.attempt,
        owner: gam.owner,
        reservationId: gam.reservationId,
      })
    ).toBe(true);
    expect(gam.attempt.fail('internal_error')).toBe(true);
    expect(harness.bridge.snapshotInventoryForTest().ticketTombstones).toBe(1);

    clock.advance(2_999);
    expect(harness.bridge.snapshotInventoryForTest().ticketTombstones).toBe(1);
    clock.advance(1);
    expect(harness.bridge.snapshotInventoryForTest().ticketTombstones).toBe(0);
  });

  it('starts the fixed ticket TTL only after posting the ready outer response', () => {
    const clock = createClock();
    const gam = createGamAttempt('aps', 71);
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      {
        claim: ({ pucSource }) => ({
          recognized: true,
          claimed: true,
          pucSource: pucSource as object,
          expiresAt: 10_000,
        }),
        mintLifecycleTicket: () => Object.freeze({ ok: true, value: LIFECYCLE_TICKET }),
        now: clock.now,
        scheduler: clock.scheduler,
      }
    );
    const port = createPort();
    port.postMessage.mockImplementation(() => clock.advance(1_000));
    expect(
      harness.bridge.registerGamAttempt({
        artifact: gam.artifact,
        attempt: gam.attempt,
        owner: gam.owner,
        reservationId: gam.reservationId,
      })
    ).toBe(true);
    harness.dispatch({
      data: exactRequest(gam.reservationId),
      ports: [port],
      source: Object.freeze({}),
      stopImmediatePropagation: vi.fn(),
    });

    expect(
      harness.bridge.recordNonemptyGam({
        artifact: gam.artifact,
        attempt: gam.attempt,
        owner: gam.owner,
        reservationId: gam.reservationId,
      })
    ).toBe(true);
    expect(harness.bridge.snapshotInventoryForTest().liveTickets).toBe(1);

    clock.advance(2_000);
    expect(harness.bridge.snapshotInventoryForTest().liveTickets).toBe(1);
    expect(gam.attempt.fail).not.toHaveBeenCalled();
    clock.advance(999);
    expect(harness.bridge.snapshotInventoryForTest().liveTickets).toBe(1);
    clock.advance(1);
    expect(harness.bridge.snapshotInventoryForTest().liveTickets).toBe(0);
    expect(gam.attempt.fail).toHaveBeenCalledWith('owner_registration_timeout');
  });

  it('keeps a reused ticket live when a cleared expiry callback from its prior issue arrives late', () => {
    let now = 0;
    const callbacks: Array<() => void> = [];
    const scheduler = {
      set: vi.fn((callback: () => void): number => {
        callbacks[callbacks.length] = callback;
        return callbacks.length;
      }),
      clear: vi.fn(),
    };
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      {
        claim: ({ pucSource }) => ({
          recognized: true,
          claimed: true,
          pucSource: pucSource as object,
          expiresAt: 10_000,
        }),
        mintLifecycleTicket: () => Object.freeze({ ok: true, value: LIFECYCLE_TICKET }),
        now: () => now,
        scheduler,
      }
    );
    const first = createGamAttempt('aps', 72);
    issueReadyTicket(harness, first, Object.freeze({ frame: 'first' }));
    const firstExpiry = callbacks[0];
    if (!firstExpiry) throw new Error('Expected the first ticket expiry callback');

    now = 3_000;
    firstExpiry();
    expect(harness.bridge.snapshotInventoryForTest().liveTickets).toBe(0);

    const second = createGamAttempt('aps', 73);
    issueReadyTicket(harness, second, Object.freeze({ frame: 'second' }));
    expect(harness.bridge.snapshotInventoryForTest().liveTickets).toBe(1);

    now = 6_000;
    firstExpiry();
    expect(harness.bridge.snapshotInventoryForTest().liveTickets).toBe(1);
    expect(second.attempt.fail).not.toHaveBeenCalled();

    const secondExpiry = callbacks[1];
    if (!secondExpiry) throw new Error('Expected the reused ticket expiry callback');
    secondExpiry();
    expect(harness.bridge.snapshotInventoryForTest().liveTickets).toBe(0);
    expect(second.attempt.fail).toHaveBeenCalledWith('owner_registration_timeout');
  });

  it('fails and tombstones a ticket when the ready outer response cannot be posted', () => {
    const gam = createGamAttempt('adm', 8);
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      {
        claim: ({ pucSource }) => ({
          recognized: true,
          claimed: true,
          pucSource: pucSource as object,
          expiresAt: 10_000,
        }),
        mintLifecycleTicket: () => Object.freeze({ ok: true, value: LIFECYCLE_TICKET }),
      }
    );
    const port = createPort();
    port.postMessage.mockImplementation(() => {
      throw new Error('outer response transport failed');
    });
    expect(
      harness.bridge.registerGamAttempt({
        artifact: gam.artifact,
        attempt: gam.attempt,
        owner: gam.owner,
        reservationId: gam.reservationId,
      })
    ).toBe(true);
    harness.dispatch({
      data: exactRequest(gam.reservationId),
      ports: [port],
      source: Object.freeze({}),
      stopImmediatePropagation: vi.fn(),
    });
    expect(
      harness.bridge.recordNonemptyGam({
        artifact: gam.artifact,
        attempt: gam.attempt,
        owner: gam.owner,
        reservationId: gam.reservationId,
      })
    ).toBe(true);

    expect(gam.attempt.fail).toHaveBeenCalledWith('internal_error');
    expect(port.close).toHaveBeenCalledOnce();
    expect(harness.bridge.snapshotInventoryForTest()).toMatchObject({
      attempts: 0,
      liveTickets: 0,
      pendingClaims: 0,
      ticketTombstones: 1,
    });
  });

  it('shares ticket capacity 320 across live entries without eviction', () => {
    const clock = createClock();
    let draw = 0;
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      {
        claim: ({ pucSource }) => ({
          recognized: true,
          claimed: true,
          pucSource: pucSource as object,
          expiresAt: 10_000,
        }),
        mintLifecycleTicket: () => {
          const suffix = draw.toString(36).padStart(22, '0').slice(-22);
          draw += 1;
          return Object.freeze({ ok: true as const, value: `t1_${suffix}` });
        },
        now: clock.now,
        scheduler: clock.scheduler,
      }
    );

    for (let index = 0; index < 320; index += 1) {
      const gam = createGamAttempt('aps', 100 + index);
      expect(
        harness.bridge.registerGamAttempt({
          artifact: gam.artifact,
          attempt: gam.attempt,
          owner: gam.owner,
          reservationId: gam.reservationId,
        })
      ).toBe(true);
      const port = createPort();
      harness.dispatch({
        data: exactRequest(gam.reservationId),
        ports: [port],
        source: Object.freeze({ index }),
        stopImmediatePropagation: vi.fn(),
      });
      expect(
        harness.bridge.recordNonemptyGam({
          artifact: gam.artifact,
          attempt: gam.attempt,
          owner: gam.owner,
          reservationId: gam.reservationId,
        })
      ).toBe(true);
      expect(JSON.parse(String(port.postMessage.mock.calls[0]?.[0])).tsOwner.status).toBe('ready');
    }
    expect(harness.bridge.snapshotInventoryForTest()).toMatchObject({
      attempts: 320,
      liveTickets: 320,
      ticketTombstones: 0,
    });

    const overflow = createGamAttempt('aps', 999);
    const overflowPort = createPort();
    expect(
      harness.bridge.registerGamAttempt({
        artifact: overflow.artifact,
        attempt: overflow.attempt,
        owner: overflow.owner,
        reservationId: overflow.reservationId,
      })
    ).toBe(true);
    harness.dispatch({
      data: exactRequest(overflow.reservationId),
      ports: [overflowPort],
      source: Object.freeze({ overflow: true }),
      stopImmediatePropagation: vi.fn(),
    });
    expect(
      harness.bridge.recordNonemptyGam({
        artifact: overflow.artifact,
        attempt: overflow.attempt,
        owner: overflow.owner,
        reservationId: overflow.reservationId,
      })
    ).toBe(true);
    expect(overflow.attempt.fail).toHaveBeenCalledWith('capability_registry_full');
    expect(JSON.parse(String(overflowPort.postMessage.mock.calls[0]?.[0])).tsOwner.status).toBe(
      'refused'
    );
    expect(draw).toBe(320);
    expect(harness.bridge.snapshotInventoryForTest()).toMatchObject({
      attempts: 320,
      liveTickets: 320,
      ticketTombstones: 0,
    });
    harness.bridge.dispose();
  });

  it('ignores an unknown owner ticket before suppression, source, or port inspection', () => {
    const harness = createHarness(() => ({ recognized: false }));
    const stopImmediatePropagation = vi.fn();
    const ports = vi.fn(() => {
      throw new Error('unknown ticket ports must not be read');
    });
    const source = vi.fn(() => {
      throw new Error('unknown ticket source must not be read');
    });

    harness.dispatch({
      data: exactOwnerRegistration(RESERVATION_ID, 't1_0000000000000000000000'),
      stopImmediatePropagation,
      get ports() {
        return ports();
      },
      get source() {
        return source();
      },
    });

    expect(stopImmediatePropagation).not.toHaveBeenCalled();
    expect(ports).not.toHaveBeenCalled();
    expect(source).not.toHaveBeenCalled();
  });

  it('consumes one exact owner registration and retains only the kernel control endpoint', () => {
    const gam = createGamAttempt('adm', 1_001);
    const pucSource = Object.freeze({ frame: 'authoritative' });
    const retained = createPort();
    const transferred = createPort();
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      {
        claim: ({ pucSource: claimedSource }) => ({
          recognized: true,
          claimed: true,
          pucSource: claimedSource as object,
          expiresAt: 10_000,
        }),
        messageChannel: class {
          readonly port1 = retained;
          readonly port2 = transferred;
        },
        mintLifecycleTicket: () => Object.freeze({ ok: true, value: LIFECYCLE_TICKET }),
      }
    );
    issueReadyTicket(harness, gam, pucSource);
    const responsePort = createPort();
    const stopImmediatePropagation = vi.fn();

    harness.dispatch({
      data: exactOwnerRegistration(gam.reservationId),
      ports: [responsePort],
      source: pucSource,
      stopImmediatePropagation,
    });

    expect(stopImmediatePropagation).toHaveBeenCalledOnce();
    expect(gam.attempt.ownerRegistered).toHaveBeenCalledOnce();
    expect(JSON.parse(String(responsePort.postMessage.mock.calls[0]?.[0]))).toEqual({
      message: 'TS Render Owner Registered',
      adId: gam.reservationId,
      version: 1,
      lifecycleTicket: LIFECYCLE_TICKET,
    });
    expect(responsePort.postMessage.mock.calls[0]?.[1]).toEqual([transferred]);
    expect(responsePort.close).toHaveBeenCalledOnce();
    expect(transferred.close).not.toHaveBeenCalled();
    expect(retained.close).not.toHaveBeenCalled();
    expect(harness.bridge.snapshotInventoryForTest()).toMatchObject({
      attempts: 1,
      liveTickets: 0,
      ticketTombstones: 1,
    });

    expect(gam.attempt.fail('internal_error')).toBe(true);
    expect(gam.artifact.dispose).toHaveBeenCalledOnce();
    expect(retained.close).toHaveBeenCalledOnce();
    expect(transferred.close).not.toHaveBeenCalled();
  });

  it('sends exact ADM start and settles only after owner insertion and intended load', () => {
    const gam = createGamAttempt('adm', 1_011);
    const pucSource = Object.freeze({ frame: 'authoritative' });
    const controlRetained = createPort();
    const controlTransferred = createPort();
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      {
        claim: ({ pucSource: claimedSource }) => ({
          recognized: true,
          claimed: true,
          pucSource: claimedSource as object,
          expiresAt: 10_000,
        }),
        messageChannel: class {
          readonly port1 = controlRetained;
          readonly port2 = controlTransferred;
        },
        mintLifecycleTicket: () => Object.freeze({ ok: true, value: LIFECYCLE_TICKET }),
      }
    );
    issueReadyTicket(harness, gam, pucSource);
    const responsePort = createPort();

    harness.dispatch({
      data: exactOwnerRegistration(gam.reservationId),
      ports: [responsePort],
      source: pucSource,
      stopImmediatePropagation: vi.fn(),
    });

    expect(controlRetained.postMessage).toHaveBeenCalledOnce();
    expect(controlRetained.postMessage.mock.calls[0]).toEqual([
      {
        message: 'TS ADM Start',
        version: 1,
        lifecycleTicket: LIFECYCLE_TICKET,
        source: {
          type: 'adm',
          version: 1,
          adm: '<main>fictional creative</main>',
          width: 300,
          height: 250,
        },
      },
      [],
    ]);
    expect(gam.attempt.accept).not.toHaveBeenCalled();

    dispatchPortMessage(controlRetained, {
      message: 'TS Owner Inserted',
      version: 1,
      lifecycleTicket: LIFECYCLE_TICKET,
    });
    expect(gam.attempt.beginAdm).toHaveBeenCalledOnce();
    expect(gam.attempt.accept).not.toHaveBeenCalled();

    dispatchPortMessage(controlRetained, {
      message: 'TS ADM Loaded',
      version: 1,
      lifecycleTicket: LIFECYCLE_TICKET,
    });
    expect(gam.attempt.accept).toHaveBeenCalledOnce();
    expect(controlRetained.postMessage).toHaveBeenCalledTimes(2);
    expect(controlRetained.postMessage.mock.calls[1]).toEqual([
      {
        message: 'TS Owner Settled',
        version: 1,
        lifecycleTicket: LIFECYCLE_TICKET,
        outcome: 'accepted',
      },
      [],
    ]);
    expect(controlRetained.close).toHaveBeenCalledOnce();
  });

  it('resolves cache privately and sends only the resulting ADM source to the owner', () => {
    const gam = createGamAttempt('cache', 1_013);
    const pucSource = Object.freeze({ frame: 'authoritative' });
    const controlRetained = createPort();
    const controlTransferred = createPort();
    let completeResolution:
      | ((
          source: Readonly<{ adm: string; height: number; type: 'adm'; version: 1; width: number }>
        ) => boolean)
      | undefined;
    const resolveCacheAdm = vi.fn((_attempt, onResolved) => {
      completeResolution = onResolved;
      return true;
    });
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      {
        claim: ({ pucSource: claimedSource }) => ({
          recognized: true,
          claimed: true,
          pucSource: claimedSource as object,
          expiresAt: 10_000,
        }),
        messageChannel: class {
          readonly port1 = controlRetained;
          readonly port2 = controlTransferred;
        },
        mintLifecycleTicket: () => Object.freeze({ ok: true, value: LIFECYCLE_TICKET }),
        resolveCacheAdm,
      }
    );
    issueReadyTicket(harness, gam, pucSource);

    harness.dispatch({
      data: exactOwnerRegistration(gam.reservationId),
      ports: [createPort()],
      source: pucSource,
      stopImmediatePropagation: vi.fn(),
    });

    expect(resolveCacheAdm).toHaveBeenCalledWith(gam.attempt, expect.any(Function));
    expect(controlRetained.postMessage).not.toHaveBeenCalled();
    expect(
      completeResolution?.(
        Object.freeze({
          type: 'adm',
          version: 1,
          adm: '<main>resolved cache creative</main>',
          width: 300,
          height: 250,
        })
      )
    ).toBe(true);
    expect(controlRetained.postMessage.mock.calls[0]).toEqual([
      {
        message: 'TS ADM Start',
        version: 1,
        lifecycleTicket: LIFECYCLE_TICKET,
        source: {
          type: 'adm',
          version: 1,
          adm: '<main>resolved cache creative</main>',
          width: 300,
          height: 250,
        },
      },
      [],
    ]);
    expect(JSON.stringify(controlRetained.postMessage.mock.calls[0])).not.toContain('cacheId');
    expect(
      completeResolution?.(
        Object.freeze({
          type: 'adm',
          version: 1,
          adm: '<main>duplicate</main>',
          width: 300,
          height: 250,
        })
      )
    ).toBe(false);
  });

  it('sends exact APS start with one document port and accepts exact document completion', () => {
    const gam = createGamAttempt('aps', 1_012);
    const pucSource = Object.freeze({ frame: 'authoritative' });
    const controlRetained = createPort();
    const controlTransferred = createPort();
    const documentRetained = createPort();
    const documentTransferred = createPort();
    const channels = [
      { port1: controlRetained, port2: controlTransferred },
      { port1: documentRetained, port2: documentTransferred },
    ];
    let channelIndex = 0;
    const issue = vi.fn(
      (input: {
        readonly attempt: PucRenderAttempt;
        readonly port: { readonly close: () => void };
      }) => {
        expect(input.attempt.onSettled(() => input.port.close())).toBe(true);
        return Object.freeze({ ok: true as const, nonce: 'n1_abcdefghijklmnopqrstuv' });
      }
    );
    const consume = vi.fn(() => true);
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      {
        claim: ({ pucSource: claimedSource }) => ({
          recognized: true,
          claimed: true,
          pucSource: claimedSource as object,
          expiresAt: 10_000,
        }),
        messageChannel: class {
          readonly port1: unknown;
          readonly port2: unknown;

          constructor() {
            const channel = channels[channelIndex];
            channelIndex += 1;
            if (!channel) throw new Error('Unexpected extra MessageChannel');
            this.port1 = channel.port1;
            this.port2 = channel.port2;
          }
        },
        mintLifecycleTicket: () => Object.freeze({ ok: true, value: LIFECYCLE_TICKET }),
        publisherOrigin: 'https://publisher.example',
        rendererNonces: Object.freeze({ issue, consume }),
        rendererUrl: 'https://publisher.example/integrations/aps/renderer/v1',
      }
    );
    issueReadyTicket(harness, gam, pucSource);

    harness.dispatch({
      data: exactOwnerRegistration(gam.reservationId),
      ports: [createPort()],
      source: pucSource,
      stopImmediatePropagation: vi.fn(),
    });

    expect(channelIndex).toBe(2);
    expect(issue).toHaveBeenCalledOnce();
    expect(controlRetained.postMessage).toHaveBeenCalledOnce();
    expect(controlRetained.postMessage.mock.calls[0]).toEqual([
      {
        message: 'TS APS Start',
        version: 1,
        lifecycleTicket: LIFECYCLE_TICKET,
        rendererUrl: 'https://publisher.example/integrations/aps/renderer/v1',
        envelope: {
          version: 1,
          nonce: 'n1_abcdefghijklmnopqrstuv',
          publisherOrigin: 'https://publisher.example',
          renderer: {
            type: 'aps',
            version: 1,
            accountId: 'publisher-account',
            bidId: 'bid-1',
            tagType: 'iframe',
            creativeUrl: 'https://creative.example/render',
            width: 300,
            height: 250,
            aaxResponse: 'renderer-envelope',
          },
        },
      },
      [documentTransferred],
    ]);

    dispatchPortMessage(documentRetained, {
      message: 'TS APS Document Accepted',
      version: 1,
      nonce: 'n1_abcdefghijklmnopqrstuv',
    });
    expect(consume).not.toHaveBeenCalled();
    expect(gam.attempt.apsDocumentAccepted).not.toHaveBeenCalled();

    dispatchPortMessage(documentRetained, {
      message: 'TS APS Runner Loaded',
      version: 1,
      nonce: 'n1_abcdefghijklmnopqrstuv',
    });
    dispatchPortMessage(documentRetained, {
      message: 'TS APS Render Completed',
      version: 1,
      nonce: 'n1_abcdefghijklmnopqrstuv',
    });
    expect(gam.attempt.accept).not.toHaveBeenCalled();

    // Control and document messages travel over different ports, so delivery order
    // is not defined even though the owner posts insertion before handing off.
    dispatchPortMessage(controlRetained, {
      message: 'TS Owner Inserted',
      version: 1,
      lifecycleTicket: LIFECYCLE_TICKET,
    });
    expect(gam.attempt.beginApsDocument).toHaveBeenCalledOnce();
    expect(consume).toHaveBeenCalledOnce();
    expect(gam.attempt.apsDocumentAccepted).toHaveBeenCalledOnce();
    expect(gam.attempt.accept).toHaveBeenCalledOnce();
    expect(controlRetained.postMessage.mock.calls[1]).toEqual([
      {
        message: 'TS Owner Settled',
        version: 1,
        lifecycleTicket: LIFECYCLE_TICKET,
        outcome: 'accepted',
      },
      [],
    ]);
    expect(controlRetained.close).toHaveBeenCalledOnce();
    expect(documentRetained.close).toHaveBeenCalledOnce();
    expect(controlTransferred.close).not.toHaveBeenCalled();
    expect(documentTransferred.close).not.toHaveBeenCalled();
  });

  it('suppresses, refuses, and invalidates a live ticket used from the wrong source', () => {
    const gam = createGamAttempt('adm', 1_002);
    const pucSource = Object.freeze({ frame: 'authoritative' });
    let channels = 0;
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      {
        claim: ({ pucSource: claimedSource }) => ({
          recognized: true,
          claimed: true,
          pucSource: claimedSource as object,
          expiresAt: 10_000,
        }),
        messageChannel: class {
          constructor() {
            channels += 1;
          }

          readonly port1 = createPort();
          readonly port2 = createPort();
        },
        mintLifecycleTicket: () => Object.freeze({ ok: true, value: LIFECYCLE_TICKET }),
      }
    );
    issueReadyTicket(harness, gam, pucSource);
    const wrongSourcePort = createPort();

    harness.dispatch({
      data: exactOwnerRegistration(gam.reservationId),
      ports: [wrongSourcePort],
      source: Object.freeze({ frame: 'wrong' }),
      stopImmediatePropagation: vi.fn(),
    });

    expect(JSON.parse(String(wrongSourcePort.postMessage.mock.calls[0]?.[0]))).toEqual({
      message: 'TS Render Owner Refused',
      adId: gam.reservationId,
      version: 1,
    });
    expect(wrongSourcePort.close).toHaveBeenCalledOnce();
    expect(channels).toBe(0);
    expect(gam.attempt.fail).toHaveBeenCalledWith('bridge_id_mismatch');
    expect(harness.bridge.snapshotInventoryForTest()).toMatchObject({
      attempts: 0,
      liveTickets: 0,
      ticketTombstones: 1,
    });

    const replayPort = createPort();
    const stopReplay = vi.fn();
    harness.dispatch({
      data: exactOwnerRegistration(gam.reservationId),
      ports: [replayPort],
      source: pucSource,
      stopImmediatePropagation: stopReplay,
    });
    expect(stopReplay).toHaveBeenCalledOnce();
    expect(JSON.parse(String(replayPort.postMessage.mock.calls[0]?.[0]))).toMatchObject({
      message: 'TS Render Owner Refused',
      adId: gam.reservationId,
    });
    expect(replayPort.close).toHaveBeenCalledOnce();
    expect(channels).toBe(0);
  });

  it('invalidates a live owner ticket on an extended shape or wrong port count', () => {
    const gam = createGamAttempt('aps', 1_003);
    const pucSource = Object.freeze({ frame: 'authoritative' });
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      {
        claim: ({ pucSource: claimedSource }) => ({
          recognized: true,
          claimed: true,
          pucSource: claimedSource as object,
          expiresAt: 10_000,
        }),
        mintLifecycleTicket: () => Object.freeze({ ok: true, value: LIFECYCLE_TICKET }),
      }
    );
    issueReadyTicket(harness, gam, pucSource);
    const first = createPort();
    const second = createPort();
    const stopImmediatePropagation = vi.fn();

    harness.dispatch({
      data: JSON.stringify({
        message: 'TS Render Owner Register',
        adId: gam.reservationId,
        version: 1,
        lifecycleTicket: LIFECYCLE_TICKET,
        extra: true,
      }),
      ports: [first, second],
      source: pucSource,
      stopImmediatePropagation,
    });

    expect(stopImmediatePropagation).toHaveBeenCalledOnce();
    expect(JSON.parse(String(first.postMessage.mock.calls[0]?.[0]))).toEqual({
      message: 'TS Render Owner Refused',
      adId: gam.reservationId,
      version: 1,
    });
    expect(first.postMessage.mock.calls[0]?.[1]).toEqual([]);
    expect(first.close).toHaveBeenCalledOnce();
    expect(second.close).toHaveBeenCalledOnce();
    expect(gam.attempt.fail).toHaveBeenCalledWith('bridge_id_mismatch');
    expect(harness.bridge.snapshotInventoryForTest().ticketTombstones).toBe(1);
  });

  it('tombstones a posted ticket when its expiry scheduler cannot arm', () => {
    let now = 0;
    const gam = createGamAttempt('aps', 81);
    const harness = createHarness(
      () => ({ recognized: true, state: 'renderable', expiresAt: 10_000 }),
      {
        claim: ({ pucSource }) => ({
          recognized: true,
          claimed: true,
          pucSource: pucSource as object,
          expiresAt: 10_000,
        }),
        mintLifecycleTicket: () => Object.freeze({ ok: true, value: LIFECYCLE_TICKET }),
        now: () => now,
        scheduler: {
          clear: vi.fn(),
          set: vi.fn(() => undefined),
        },
      }
    );
    const port = createPort();
    expect(
      harness.bridge.registerGamAttempt({
        artifact: gam.artifact,
        attempt: gam.attempt,
        owner: gam.owner,
        reservationId: gam.reservationId,
      })
    ).toBe(true);
    harness.dispatch({
      data: exactRequest(gam.reservationId),
      ports: [port],
      source: Object.freeze({}),
      stopImmediatePropagation: vi.fn(),
    });

    expect(
      harness.bridge.recordNonemptyGam({
        artifact: gam.artifact,
        attempt: gam.attempt,
        owner: gam.owner,
        reservationId: gam.reservationId,
      })
    ).toBe(true);
    expect(port.postMessage).toHaveBeenCalledOnce();
    expect(gam.attempt.fail).toHaveBeenCalledWith('internal_error');
    expect(harness.bridge.snapshotInventoryForTest()).toMatchObject({
      liveTickets: 0,
      ticketTombstones: 1,
    });

    now = 3_000;
    const latePort = createPort();
    harness.dispatch({
      data: exactOwnerRegistration(gam.reservationId, LIFECYCLE_TICKET),
      ports: [latePort],
      source: Object.freeze({}),
      stopImmediatePropagation: vi.fn(),
    });
    expect(harness.bridge.snapshotInventoryForTest().ticketTombstones).toBe(0);
    expect(latePort.postMessage).not.toHaveBeenCalled();
    expect(latePort.close).not.toHaveBeenCalled();
  });
});
