import { describe, expect, it, vi } from 'vitest';
import { JSDOM } from 'jsdom';

import type { FirstDisplayGptBoundCycleV1 } from '../../src/first_display/adapters/googletag';
import { createFirstDisplayRenderBridge } from '../../src/first_display/render_bridge';
import { PUC_DYNAMIC_OWNER } from '../../src/kernel/contracts/puc_dynamic_owner';

const RESERVATION_ID = `r1_${'a'.repeat(22)}`;

class FakePort {
  public readonly addEventListener = vi.fn(
    (name: string, listener: (event: { data: unknown; ports: readonly FakePort[] }) => void) => {
      this.listeners.set(name, listener);
    }
  );
  public readonly close = vi.fn();
  public readonly postMessage = vi.fn();
  public readonly removeEventListener = vi.fn();
  public readonly start = vi.fn();
  private readonly listeners = new Map<
    string,
    (event: { data: unknown; ports: readonly FakePort[] }) => void
  >();

  public dispatch(data: unknown, ports: readonly FakePort[] = []): void {
    const listener = this.listeners.get('message');
    if (!listener) throw new Error('expected a live port listener');
    listener({ data, ports });
  }

  public dispatchError(): void {
    const listener = this.listeners.get('messageerror');
    if (!listener) throw new Error('expected a live port error listener');
    listener({ data: undefined, ports: [] });
  }
}

function fixture(kind: 'adm' | 'aps' = 'adm') {
  const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
    url: 'https://publisher.example/',
  });
  const element = dom.window.document.getElementById('slot-1');
  if (!(element instanceof dom.window.HTMLElement)) throw new Error('missing fixture element');
  const bid = Object.freeze({
    candidateId: 'candidate001',
    slot: 'slot-1',
    provider: 'example',
    upstreamBidId: 'upstream-1',
    cpm: 1.25,
    currency: 'USD' as const,
    targeting: Object.freeze({}),
    rendererReservationId: RESERVATION_ID,
    renderSource: Object.freeze(
      kind === 'adm'
        ? {
            type: 'adm' as const,
            version: 1 as const,
            adm: '<main>fictional creative</main>',
            width: 300,
            height: 250,
          }
        : {
            type: 'aps' as const,
            version: 1 as const,
            accountId: 'publisher-account',
            bidId: 'bid-1',
            tagType: 'iframe' as const,
            creativeUrl: 'https://creative.example/render',
            width: 300,
            height: 250,
            aaxResponse: 'renderer-envelope',
          }
    ),
  });
  const placement = Object.freeze({
    slot: 'slot-1',
    gamUnitPath: '/123/example',
    divId: 'slot-1',
    formats: Object.freeze([Object.freeze([300, 250] as const)]),
    targeting: Object.freeze({}),
  });
  let cycleCurrent = true;
  const cycle: FirstDisplayGptBoundCycleV1 = Object.freeze({
    bid,
    element,
    ownership: 'trusted_server',
    physicalSlot: {},
    isCurrent: () => cycleCurrent,
    placement,
    slotId: 'slot-1',
    traceToken: 'gt1_1',
  });
  return { cycle, dom, element, invalidateCycle: () => (cycleCurrent = false) };
}

function harness(kind: 'adm' | 'aps' = 'adm', onNativeMutation?: () => boolean) {
  const value = fixture(kind);
  let listener: ((event: Record<string, unknown>) => void) | undefined;
  const target = {
    addEventListener: vi.fn(
      (name: string, next: (event: Record<string, unknown>) => void, capture: boolean) => {
        expect(name).toBe('message');
        expect(capture).toBe(true);
        listener = next;
      }
    ),
    removeEventListener: vi.fn(),
  };
  const channels: Array<{ port1: FakePort; port2: FakePort }> = [];
  const timers = new Map<object, Readonly<{ callback: () => void; delayMs: number }>>();
  let randomByte = 1;
  let now = 0;
  const bridge = createFirstDisplayRenderBridge({
    getAps: () =>
      kind === 'aps'
        ? Object.freeze({
            version: 1 as const,
            id: 'aps' as const,
            publisherOrigin: 'https://publisher.example',
            rendererUrl: 'https://publisher.example/integrations/aps/renderer/v1',
            sandbox:
              'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation',
            permanentSandbox:
              'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-same-origin allow-scripts allow-top-navigation-by-user-activation',
            deadlines: Object.freeze({
              insertionMs: 1_000 as const,
              documentAcceptanceMs: 3_000 as const,
              completionMs: 10_000 as const,
              ownerSettlementMs: 20_000 as const,
            }),
            isReservationId: (candidate: unknown): candidate is string =>
              typeof candidate === 'string' && /^r1_[A-Za-z0-9_-]{22}$/.test(candidate),
            isLifecycleTicket: (candidate: unknown): candidate is string =>
              typeof candidate === 'string' && /^t1_[A-Za-z0-9_-]{22}$/.test(candidate),
            isBootstrapNonce: (candidate: unknown): candidate is string =>
              typeof candidate === 'string' && /^b1_[A-Za-z0-9_-]{22}$/.test(candidate),
            isRendererNonce: (candidate: unknown): candidate is string =>
              typeof candidate === 'string' && /^n1_[A-Za-z0-9_-]{22}$/.test(candidate),
            generateDocuments: (
              _renderer: unknown,
              bootstrapNonce: string,
              rendererNonce: string
            ) =>
              Object.freeze({
                outerUrl:
                  'data:text/html;charset=utf-8,' +
                  encodeURIComponent(`rendererNonce=${rendererNonce}`) +
                  `#${bootstrapNonce}`,
              }),
            createRenderBridge: () => {
              throw new Error('the full bridge fixture already owns construction');
            },
            parseDocumentMessage: (candidate: unknown, nonce: string) => {
              const message = candidate as Record<string, unknown>;
              if (message.version !== 1 || message.nonce !== nonce) return undefined;
              if (message.message === 'TS APS Document Accepted') {
                return Object.freeze({ kind: 'document_accepted' as const });
              }
              if (message.message === 'TS APS Runner Loaded') {
                return Object.freeze({ kind: 'runner_loaded' as const });
              }
              if (message.message === 'TS APS Render Completed') {
                return Object.freeze({ kind: 'render_completed' as const });
              }
              if (
                message.message === 'TS APS Render Failed' &&
                (message.reason === 'descriptor_invalid' ||
                  message.reason === 'runner_no_load' ||
                  message.reason === 'runner_failed')
              ) {
                return Object.freeze({
                  kind: 'render_failed' as const,
                  reason: message.reason,
                });
              }
              return undefined;
            },
          })
        : undefined,
    now: () => now,
    browser: target as unknown as Window,
    clearTimer: (handle) => timers.delete(handle as object),
    createChannel: () => {
      const channel = { port1: new FakePort(), port2: new FakePort() };
      channels.push(channel);
      return channel;
    },
    document: value.dom.window.document,
    fillRandom: (bytes) => {
      bytes.fill(randomByte);
      randomByte += 1;
    },
    ...(onNativeMutation ? { onNativeMutation } : {}),
    setTimer: (callback, delayMs) => {
      const handle = {};
      timers.set(handle, Object.freeze({ callback, delayMs }));
      return handle;
    },
  });
  const terminals: string[] = [];
  const terminalFacts: Array<readonly [string, string | null]> = [];
  expect(
    bridge.bind(value.cycle, (result, reason) => {
      terminals.push(result);
      terminalFacts.push([result, reason]);
    })
  ).toBe(true);
  const dispatch = (event: Record<string, unknown>): void => {
    if (!listener) throw new Error('expected capture listener');
    listener(event);
  };
  const fire = (delayMs: number): void => {
    const entry = [...timers.entries()].find(([, timer]) => timer.delayMs === delayMs);
    if (!entry) throw new Error(`missing ${delayMs}ms timer`);
    timers.delete(entry[0]);
    now += delayMs;
    entry[1].callback();
  };
  const fireLast = (delayMs: number): void => {
    const entries = [...timers.entries()].filter(([, timer]) => timer.delayMs === delayMs);
    const entry = entries[entries.length - 1];
    if (!entry) throw new Error(`missing ${delayMs}ms timer`);
    timers.delete(entry[0]);
    now += delayMs;
    entry[1].callback();
  };
  return {
    ...value,
    bridge,
    channels,
    dispatch,
    fire,
    fireLast,
    target,
    terminalFacts,
    terminals,
    timers,
  };
}

function requestEvent(
  responsePort: FakePort,
  options: Readonly<{ adId?: string; data?: unknown; source?: object }> = {}
) {
  return {
    data:
      options.data ??
      JSON.stringify({
        message: 'Prebid Request',
        adId: options.adId ?? RESERVATION_ID,
        adServerDomain: 'ads.example',
      }),
    ports: [responsePort],
    source: options.source ?? {},
    stopImmediatePropagation: vi.fn(),
  };
}

function ownerRegistration(ticket: string, responsePort: FakePort, source: object) {
  return {
    data: JSON.stringify({
      message: 'TS Render Owner Register',
      adId: RESERVATION_ID,
      version: 1,
      lifecycleTicket: ticket,
    }),
    ports: [responsePort],
    source,
    stopImmediatePropagation: vi.fn(),
  };
}

function collapsedShell(h: ReturnType<typeof harness>) {
  const wrapper = h.dom.window.document.createElement('div');
  const frame = h.dom.window.document.createElement('iframe');
  wrapper.style.width = '1px';
  wrapper.style.height = '1px';
  frame.setAttribute('width', '1');
  frame.setAttribute('height', '1');
  frame.style.width = '1px';
  frame.style.height = '1px';
  wrapper.appendChild(frame);
  h.dom.window.document.body.appendChild(wrapper);
  return { frame, wrapper };
}

function registerOwner(h: ReturnType<typeof harness>) {
  const source = {};
  const responsePort = new FakePort();
  h.dispatch(requestEvent(responsePort, { source }));
  expect(h.bridge.recordGam(h.cycle, 'nonempty_gam')).toBe(true);
  const outer = JSON.parse(responsePort.postMessage.mock.calls[0]?.[0] as string);
  const ticket = outer.tsOwner.lifecycleTicket as string;
  const registrationPort = new FakePort();
  h.dispatch(ownerRegistration(ticket, registrationPort, source));
  return { source, ticket };
}

function startApsDocument(h: ReturnType<typeof harness>) {
  const frame = h.element.querySelector<HTMLIFrameElement>('iframe');
  if (!frame?.contentWindow) throw new Error('expected the top-page APS frame');
  const bootstrapNonce = new URL(frame.src).hash.slice(1);
  const postMessage = vi
    .spyOn(frame.contentWindow, 'postMessage')
    .mockImplementation(() => undefined);
  h.dispatch({
    data: JSON.stringify({
      message: 'TS APS Bootstrap Ready',
      version: 1,
      bootstrapNonce,
    }),
    origin: 'null',
    ports: [],
    source: frame.contentWindow,
  });
  const navigation = JSON.parse(postMessage.mock.calls[0]?.[0] as string) as Record<
    string,
    unknown
  >;
  const decodedOuter = decodeURIComponent(
    (navigation.containerUrl as string)
      .slice('data:text/html;charset=utf-8,'.length)
      .split('#')[0] ?? ''
  );
  const nonce = /rendererNonce=(n1_[A-Za-z0-9_-]{22})/.exec(decodedOuter)?.[1];
  if (!nonce) throw new Error('expected the renderer nonce');
  const documentPort = new FakePort();
  h.dispatch({
    data: JSON.stringify({
      message: 'TS APS Container Ready',
      version: 1,
      bootstrapNonce,
      rendererNonce: nonce,
    }),
    origin: 'null',
    ports: [documentPort],
    source: frame.contentWindow,
  });
  return { documentPort, frame, nonce };
}

function acceptPucAps(h: ReturnType<typeof harness>): HTMLIFrameElement {
  registerOwner(h);
  const { documentPort, frame, nonce } = startApsDocument(h);
  documentPort.dispatch({
    message: 'TS APS Document Accepted',
    version: 1,
    nonce,
  });
  documentPort.dispatch({
    message: 'TS APS Render Completed',
    version: 1,
    nonce,
  });
  expect(h.terminals).toEqual(['accepted']);
  return frame;
}

describe('bounded first-display render bridge', () => {
  it('observes admitted bridge activity and terminal tombstone expiry', () => {
    const mutations = vi.fn(() => true);
    const h = harness('adm', mutations);
    const shell = collapsedShell(h);
    const owner = registerOwner(h);
    h.channels[0]?.port1.dispatch({
      message: 'TS Owner Inserted',
      version: 1,
      lifecycleTicket: owner.ticket,
    });
    h.channels[0]?.port1.dispatch({
      message: 'TS ADM Loaded',
      version: 1,
      lifecycleTicket: owner.ticket,
    });
    expect(h.terminals).toEqual(['accepted']);
    expect(shell.frame.isConnected).toBe(true);

    mutations.mockClear();
    h.fire(3_000);
    expect(mutations).toHaveBeenCalledOnce();
  });

  it('passes native ids through and suppresses malformed recognized TS requests', () => {
    const h = harness();
    const nativePort = new FakePort();
    const native = requestEvent(nativePort, { adId: `r1_${'z'.repeat(22)}` });
    h.dispatch(native);
    expect(native.stopImmediatePropagation).not.toHaveBeenCalled();
    expect(nativePort.postMessage).not.toHaveBeenCalled();

    const refusedPort = new FakePort();
    const malformed = requestEvent(refusedPort, {
      data: {
        message: 'Prebid Request',
        adId: RESERVATION_ID,
        adServerDomain: 'ads.example',
      },
    });
    h.dispatch(malformed);
    expect(malformed.stopImmediatePropagation).toHaveBeenCalledOnce();
    expect(JSON.parse(refusedPort.postMessage.mock.calls[0]?.[0] as string)).toEqual({
      message: 'Prebid Response',
      adId: RESERVATION_ID,
      rendererVersion: '4',
      tsOwner: { version: 1, status: 'refused' },
    });
    expect(refusedPort.close).toHaveBeenCalledOnce();
  });

  it('never recognizes inherited or accessor-backed message routing fields', () => {
    const h = harness();
    const read = vi.fn(() =>
      JSON.stringify({
        message: 'Prebid Request',
        adId: RESERVATION_ID,
        adServerDomain: 'ads.example',
      })
    );
    const event = Object.create(
      Object.defineProperty({}, 'data', {
        configurable: true,
        get: read,
      })
    ) as Record<string, unknown>;
    event.ports = [new FakePort()];
    event.source = {};
    event.stopImmediatePropagation = vi.fn();

    h.dispatch(event);
    expect(read).not.toHaveBeenCalled();
    expect(event.stopImmediatePropagation).not.toHaveBeenCalled();
  });

  it('joins an exact claim and nonempty GAM cycle through one-use ADM owner control', () => {
    const h = harness('adm');
    const shell = collapsedShell(h);
    const source = shell.frame.contentWindow!;
    const responsePort = new FakePort();
    const claim = requestEvent(responsePort, { source });
    h.dispatch(claim);
    expect(claim.stopImmediatePropagation).toHaveBeenCalledOnce();
    expect(responsePort.postMessage).not.toHaveBeenCalled();

    expect(h.bridge.recordGam(h.cycle, 'nonempty_gam')).toBe(true);
    const outer = JSON.parse(responsePort.postMessage.mock.calls[0]?.[0] as string) as Record<
      string,
      unknown
    >;
    expect(outer).toMatchObject({
      message: 'Prebid Response',
      adId: RESERVATION_ID,
      renderer: PUC_DYNAMIC_OWNER,
      rendererVersion: '4',
      tsOwner: { version: 1, status: 'ready', kind: 'adm' },
    });
    const ticket = (outer.tsOwner as Record<string, unknown>).lifecycleTicket;
    expect(ticket).toMatch(/^t1_[A-Za-z0-9_-]{22}$/);
    expect(responsePort.close).toHaveBeenCalledOnce();
    expect(shell.frame.style.width).toBe('300px');
    expect(shell.frame.style.height).toBe('250px');
    expect(shell.wrapper.style.width).toBe('300px');
    expect(shell.wrapper.style.height).toBe('250px');

    const registrationPort = new FakePort();
    const registration = ownerRegistration(ticket as string, registrationPort, source);
    h.dispatch(registration);
    expect(registration.stopImmediatePropagation).toHaveBeenCalledOnce();
    const registered = JSON.parse(registrationPort.postMessage.mock.calls[0]?.[0] as string);
    expect(registered).toEqual({
      message: 'TS Render Owner Registered',
      adId: RESERVATION_ID,
      version: 1,
      lifecycleTicket: ticket,
    });
    expect(registrationPort.postMessage.mock.calls[0]?.[1]).toEqual([h.channels[0]?.port2]);
    expect(h.channels[0]?.port1.postMessage.mock.calls[0]?.[0]).toEqual({
      message: 'TS ADM Start',
      version: 1,
      lifecycleTicket: ticket,
      source: h.cycle.bid.renderSource,
    });

    h.channels[0]?.port1.dispatch({
      message: 'TS Owner Inserted',
      version: 1,
      lifecycleTicket: ticket,
    });
    expect(h.terminals).toEqual([]);
    h.channels[0]?.port1.dispatch({
      message: 'TS ADM Loaded',
      version: 1,
      lifecycleTicket: ticket,
    });
    expect(h.terminals).toEqual(['accepted']);
    const controlCalls = h.channels[0]?.port1.postMessage.mock.calls ?? [];
    expect(controlCalls[controlCalls.length - 1]?.[0]).toEqual({
      message: 'TS Owner Settled',
      version: 1,
      lifecycleTicket: ticket,
      outcome: 'accepted',
    });

    const replayPort = new FakePort();
    const replay = ownerRegistration(ticket as string, replayPort, source);
    h.dispatch(replay);
    expect(replay.stopImmediatePropagation).toHaveBeenCalledOnce();
    expect(JSON.parse(replayPort.postMessage.mock.calls[0]?.[0] as string)).toEqual({
      message: 'TS Render Owner Refused',
      adId: RESERVATION_ID,
      version: 1,
    });
  });

  it('refuses delayed APS owner registration after a later physical GPT request', () => {
    const h = harness('aps');
    const source = {};
    const responsePort = new FakePort();
    h.dispatch(requestEvent(responsePort, { source }));
    expect(h.bridge.recordGam(h.cycle, 'nonempty_gam')).toBe(true);
    const response = JSON.parse(responsePort.postMessage.mock.calls[0]?.[0] as string);

    h.invalidateCycle();
    const registrationPort = new FakePort();
    h.dispatch(
      ownerRegistration(response.tsOwner.lifecycleTicket as string, registrationPort, source)
    );

    expect(h.terminals).toEqual(['failed']);
    expect(h.terminalFacts).toEqual([['failed', 'slot_unresolved']]);
    expect(h.element.querySelector('iframe')).toBeNull();
  });

  it('never resizes an anchored or already-expanded PUC shell', () => {
    const cases = [
      {
        mutate: ({ wrapper }: ReturnType<typeof collapsedShell>) => {
          wrapper.setAttribute('data-anchor-status', 'displayed');
        },
        wrapperWidth: '1px',
      },
      {
        mutate: ({ wrapper }: ReturnType<typeof collapsedShell>) => {
          wrapper.style.width = '2px';
        },
        wrapperWidth: '2px',
      },
    ];
    for (const { mutate, wrapperWidth } of cases) {
      const h = harness('adm');
      const shell = collapsedShell(h);
      mutate(shell);
      const responsePort = new FakePort();
      h.dispatch(requestEvent(responsePort, { source: shell.frame.contentWindow! }));
      expect(h.bridge.recordGam(h.cycle, 'nonempty_gam')).toBe(true);
      expect(shell.frame.style.width).toBe('1px');
      expect(shell.wrapper.style.width).toBe(wrapperWidth);
      h.bridge.dispose();
    }
  });

  it('requires APS document acceptance and completion in a hidden top-page PUC overlay', () => {
    const h = harness('aps');
    h.element.innerHTML = '<span>publisher GAM content</span>';
    const source = {};
    const responsePort = new FakePort();
    h.dispatch(requestEvent(responsePort, { source }));
    expect(h.bridge.recordGam(h.cycle, 'nonempty_gam')).toBe(true);
    const outer = JSON.parse(responsePort.postMessage.mock.calls[0]?.[0] as string);
    const ticket = outer.tsOwner.lifecycleTicket as string;
    const registrationPort = new FakePort();
    h.dispatch(ownerRegistration(ticket, registrationPort, source));

    const start = h.channels[0]?.port1.postMessage.mock.calls[0]?.[0] as Record<string, unknown>;
    expect(start).toEqual({
      message: 'TS APS Top Mount Started',
      version: 1,
      lifecycleTicket: ticket,
    });
    expect(h.channels[0]?.port1.postMessage.mock.calls[0]?.[1]).toEqual([]);
    expect(h.channels).toHaveLength(1);
    expect(h.element.querySelector('span')?.textContent).toBe('publisher GAM content');
    const { documentPort, frame, nonce } = startApsDocument(h);
    expect(frame.parentNode).toBe(h.element);
    expect(frame.style.position).toBe('absolute');
    expect(frame.style.visibility).toBe('hidden');
    expect(h.element.style.position).toBe('relative');

    documentPort.dispatch({
      message: 'TS APS Document Accepted',
      version: 1,
      nonce,
    });
    documentPort.dispatch({
      message: 'TS APS Runner Loaded',
      version: 1,
      nonce,
    });
    expect(h.terminals).toEqual([]);
    expect(frame.style.visibility).toBe('hidden');
    documentPort.dispatch({
      message: 'TS APS Render Completed',
      version: 1,
      nonce,
    });
    expect(h.terminals).toEqual(['accepted']);
    expect(frame.style.visibility).toBe('visible');
    expect(h.element.querySelector('span')?.textContent).toBe('publisher GAM content');
    expect(h.channels[0]?.port1.postMessage.mock.calls[1]?.[0]).toEqual({
      message: 'TS Owner Settled',
      version: 1,
      lifecycleTicket: ticket,
      outcome: 'accepted',
    });
    expect(h.bridge.retire(h.cycle)).toBe(true);
    expect(h.bridge.retire(h.cycle)).toBe(false);
    expect(frame.isConnected).toBe(false);
    expect(h.element.style.getPropertyValue('position')).toBe('');
    expect(h.element.querySelector('span')?.textContent).toBe('publisher GAM content');
    expect(h.terminals).toEqual(['accepted']);
  });

  it.each(['removed', 'reparented', 'host_replaced'] as const)(
    'retires an accepted APS overlay before handoff when its DOM is %s',
    (mutation) => {
      const h = harness('aps');
      h.element.innerHTML = '<span>publisher</span>';
      const frame = acceptPucAps(h);
      const movedHost = h.dom.window.document.createElement('div');
      movedHost.id = 'moved-host';
      h.dom.window.document.body.appendChild(movedHost);

      if (mutation === 'removed') frame.remove();
      if (mutation === 'reparented') movedHost.appendChild(frame);
      if (mutation === 'host_replaced') {
        const replacement = h.element.cloneNode(false);
        h.element.replaceWith(replacement);
      }

      expect(h.bridge.sweepCommittedArtifacts()).toBe(1);
      expect(h.bridge.sweepCommittedArtifacts()).toBe(0);
      expect(frame.isConnected).toBe(false);
      expect(h.element.style.getPropertyValue('position')).toBe('');
      expect(h.element.querySelector('span')?.textContent).toBe('publisher');
      expect(h.terminals).toEqual(['accepted']);

      h.bridge.sealTsAdmission();
      expect(h.bridge.closeIngress()).toBe(true);
      expect(h.bridge.captureHandoff()?.artifacts).toEqual([]);
      expect(h.bridge.detachCommittedArtifacts()).toBe(true);
    }
  );

  it.each(['src', 'srcdoc', 'sandbox', 'frame_style', 'host_position'] as const)(
    'retires an accepted APS overlay before handoff when its %s integrity changes',
    (mutation) => {
      const h = harness('aps');
      h.element.innerHTML = '<span>publisher</span>';
      const frame = acceptPucAps(h);

      if (mutation === 'src') frame.setAttribute('src', 'https://publisher.example/replaced');
      if (mutation === 'srcdoc') frame.srcdoc = '<!doctype html><title>Replacement</title>';
      if (mutation === 'sandbox') frame.setAttribute('sandbox', 'allow-scripts');
      if (mutation === 'frame_style') frame.style.setProperty('visibility', 'hidden');
      if (mutation === 'host_position') h.element.style.setProperty('position', 'absolute');

      expect(h.bridge.sweepCommittedArtifacts()).toBe(1);
      expect(h.bridge.sweepCommittedArtifacts()).toBe(0);
      expect(frame.isConnected).toBe(false);
      expect(h.element.querySelector('span')?.textContent).toBe('publisher');
      expect(h.terminals).toEqual(['accepted']);

      h.bridge.sealTsAdmission();
      expect(h.bridge.closeIngress()).toBe(true);
      expect(h.bridge.captureHandoff()?.artifacts).toEqual([]);
      expect(h.bridge.detachCommittedArtifacts()).toBe(true);
    }
  );

  it('renders an attributable empty-GAM ADM fallback directly into the bound element', () => {
    const h = harness('adm');
    expect(h.bridge.recordGam(h.cycle, 'gam_empty')).toBe(true);
    const frame = h.element.querySelector('iframe');
    expect(frame).not.toBeNull();
    expect(frame?.srcdoc).toContain('fictional creative');
    expect(frame?.getAttribute('sandbox')).toBe(
      'allow-forms allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation'
    );
    expect(h.terminals).toEqual([]);
    frame?.dispatchEvent(new h.dom.window.Event('load'));
    expect(h.terminals).toEqual(['accepted']);
    expect(h.element.contains(frame)).toBe(true);
    expect(frame?.onload).toBeNull();
    expect(frame?.onerror).toBeNull();
    frame?.dispatchEvent(new h.dom.window.Event('load'));
    expect(h.terminals).toEqual(['accepted']);
    h.bridge.dispose();
    expect(h.element.contains(frame)).toBe(false);
  });

  it('renders an attributable empty-GAM APS fallback through the exact document channel', () => {
    const h = harness('aps');
    expect(h.bridge.recordGam(h.cycle, 'gam_empty')).toBe(true);
    const frame = h.element.querySelector('iframe');
    expect(frame).not.toBeNull();
    const bootstrapNonce = new URL(frame?.src ?? '').hash.slice(1);
    expect(bootstrapNonce).toMatch(/^b1_[A-Za-z0-9_-]{22}$/);
    expect(h.channels).toHaveLength(0);
    const postMessage = vi
      .spyOn(frame!.contentWindow!, 'postMessage')
      .mockImplementation(() => undefined);
    h.dispatch({
      data: JSON.stringify({
        message: 'TS APS Bootstrap Ready',
        version: 1,
        bootstrapNonce,
      }),
      origin: 'null',
      ports: [],
      source: frame?.contentWindow,
    });
    const navigation = JSON.parse(postMessage.mock.calls[0]?.[0] as string) as Record<
      string,
      unknown
    >;
    expect(navigation).toMatchObject({
      message: 'TS APS Bootstrap Navigate',
      version: 1,
      bootstrapNonce,
      containerUrl: expect.stringMatching(/^data:text\/html;charset=utf-8,/),
    });
    expect(postMessage.mock.calls[0]?.slice(1)).toEqual(['*', []]);
    expect(frame?.getAttribute('sandbox')).toBe(
      'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-same-origin allow-scripts allow-top-navigation-by-user-activation'
    );
    const decodedOuter = decodeURIComponent(
      (navigation.containerUrl as string)
        .slice('data:text/html;charset=utf-8,'.length)
        .split('#')[0] ?? ''
    );
    const nonce = /rendererNonce=(n1_[A-Za-z0-9_-]{22})/.exec(decodedOuter)?.[1];
    expect(nonce).toMatch(/^n1_[A-Za-z0-9_-]{22}$/);
    const documentPort = new FakePort();
    h.dispatch({
      data: JSON.stringify({
        message: 'TS APS Container Ready',
        version: 1,
        bootstrapNonce,
        rendererNonce: nonce,
      }),
      origin: 'null',
      ports: [documentPort],
      source: frame?.contentWindow,
    });
    expect(documentPort.postMessage).toHaveBeenCalledWith(
      {
        version: 1,
        nonce,
        publisherOrigin: 'https://publisher.example',
        renderer: h.cycle.bid.renderSource,
      },
      []
    );
    documentPort.dispatch({
      message: 'TS APS Document Accepted',
      version: 1,
      nonce,
    });
    documentPort.dispatch({
      message: 'TS APS Runner Loaded',
      version: 1,
      nonce,
    });
    expect(h.terminals).toEqual([]);
    documentPort.dispatch({
      message: 'TS APS Render Completed',
      version: 1,
      nonce,
    });
    expect(h.terminals).toEqual(['accepted']);
    expect(h.element.contains(frame)).toBe(true);
    expect(frame?.onload).toBeNull();
    expect(frame?.onerror).toBeNull();
  });

  it('fails closed when an APS document port reports a clone error', () => {
    const h = harness('aps');
    expect(h.bridge.recordGam(h.cycle, 'gam_empty')).toBe(true);
    const frame = h.element.querySelector('iframe');
    const bootstrapNonce = new URL(frame?.src ?? '').hash.slice(1);
    const postMessage = vi
      .spyOn(frame!.contentWindow!, 'postMessage')
      .mockImplementation(() => undefined);
    h.dispatch({
      data: JSON.stringify({
        message: 'TS APS Bootstrap Ready',
        version: 1,
        bootstrapNonce,
      }),
      origin: 'null',
      ports: [],
      source: frame?.contentWindow,
    });
    const navigation = JSON.parse(postMessage.mock.calls[0]?.[0] as string) as Record<
      string,
      unknown
    >;
    const decodedOuter = decodeURIComponent(
      (navigation.containerUrl as string)
        .slice('data:text/html;charset=utf-8,'.length)
        .split('#')[0] ?? ''
    );
    const rendererNonce = /rendererNonce=(n1_[A-Za-z0-9_-]{22})/.exec(decodedOuter)?.[1];
    const documentPort = new FakePort();
    h.dispatch({
      data: JSON.stringify({
        message: 'TS APS Container Ready',
        version: 1,
        bootstrapNonce,
        rendererNonce,
      }),
      origin: 'null',
      ports: [documentPort],
      source: frame?.contentWindow,
    });
    documentPort.dispatchError();
    expect(h.terminals).toEqual(['failed']);
    expect(h.element.querySelector('iframe')).toBeNull();
  });

  it('bounds claim waiting and disposes every listener, timer, port, and pending frame once', () => {
    const timeout = harness('adm');
    expect(timeout.bridge.recordGam(timeout.cycle, 'nonempty_gam')).toBe(true);
    timeout.fire(3_000);
    expect(timeout.terminals).toEqual(['failed']);
    expect(timeout.terminalFacts).toEqual([['failed', 'bridge_claim_timeout']]);

    const pending = harness('adm');
    expect(pending.bridge.recordGam(pending.cycle, 'gam_empty')).toBe(true);
    const frame = pending.element.querySelector('iframe');
    expect(frame).not.toBeNull();
    pending.bridge.dispose();
    pending.bridge.dispose();
    expect(pending.terminals).toEqual(['cancelled']);
    expect(pending.terminalFacts).toEqual([['cancelled', 'navigation_disposed']]);
    expect(pending.element.contains(frame)).toBe(false);
    expect(pending.target.removeEventListener).toHaveBeenCalledTimes(1);
    expect(pending.target.removeEventListener).toHaveBeenCalledWith(
      'message',
      expect.any(Function),
      true
    );
    expect(pending.timers).toHaveLength(0);
  });

  it('enforces ticket, insertion, ADM load, APS document, and APS completion deadlines', () => {
    const ticket = harness('adm');
    const ticketPort = new FakePort();
    ticket.dispatch(requestEvent(ticketPort));
    expect(ticket.bridge.recordGam(ticket.cycle, 'nonempty_gam')).toBe(true);
    ticket.fire(3_000);
    expect(ticket.terminals).toEqual(['failed']);
    expect(ticket.terminalFacts).toEqual([['failed', 'owner_registration_timeout']]);

    const insertion = harness('adm');
    registerOwner(insertion);
    insertion.fire(1_000);
    expect(insertion.terminals).toEqual(['failed']);
    expect(insertion.terminalFacts).toEqual([['failed', 'owner_insertion_timeout']]);

    const adm = harness('adm');
    const admOwner = registerOwner(adm);
    adm.channels[0]?.port1.dispatch({
      message: 'TS Owner Inserted',
      version: 1,
      lifecycleTicket: admOwner.ticket,
    });
    adm.fire(5_000);
    expect(adm.terminals).toEqual(['failed']);
    expect(adm.terminalFacts).toEqual([['failed', 'adm_document_no_load']]);

    const document = harness('aps');
    registerOwner(document);
    document.fireLast(3_000);
    expect(document.terminals).toEqual(['failed']);
    expect(document.terminalFacts).toEqual([['failed', 'renderer_document_no_load']]);

    const completion = harness('aps');
    registerOwner(completion);
    const { documentPort, nonce } = startApsDocument(completion);
    documentPort.dispatch({
      message: 'TS APS Document Accepted',
      version: 1,
      nonce,
    });
    completion.fire(10_000);
    expect(completion.terminals).toEqual(['failed']);
    expect(completion.terminalFacts).toEqual([['failed', 'runner_failed']]);
  });

  it('seals only after every attempt is terminal and refuses later authority', () => {
    const active = harness('adm');
    expect(() => active.bridge.sealTsAdmission()).toThrow('tsjs');

    const terminal = harness('adm');
    expect(terminal.bridge.recordGam(terminal.cycle, 'gam_empty')).toBe(true);
    terminal.element.querySelector('iframe')?.dispatchEvent(new terminal.dom.window.Event('load'));
    expect(() => terminal.bridge.sealTsAdmission()).not.toThrow();
    expect(terminal.bridge.bind(terminal.cycle, () => undefined)).toBe(false);
  });

  it('captures and detaches an accepted direct frame without retaining provisional authority', () => {
    const h = harness('adm');
    expect(h.bridge.recordGam(h.cycle, 'gam_empty')).toBe(true);
    const frame = h.element.querySelector('iframe');
    frame?.dispatchEvent(new h.dom.window.Event('load'));
    h.bridge.sealTsAdmission();

    expect(h.bridge.closeIngress()).toBe(true);
    expect(h.bridge.captureHandoff()).toEqual({
      artifacts: [
        {
          hostPosition: null,
          hostPositionPriority: null,
          identity: frame,
          kind: 'gpt_adm',
          owner: 'trusted_server',
          slotId: 'slot-1',
          token: RESERVATION_ID,
        },
      ],
      clockEpochMs: 0,
      nextReservationOrdinal: 2,
      nextTicketOrdinal: 1,
      tombstones: [
        {
          expiresAtMs: 900_000,
          kind: 'reservation',
          ordinal: 1,
          value: RESERVATION_ID,
        },
      ],
    });
    expect(h.bridge.detachCommittedArtifacts()).toBe(true);
    expect(h.bridge.detachCommittedArtifacts()).toBe(false);
    h.bridge.dispose();

    expect(frame?.isConnected).toBe(true);
    expect(h.target.removeEventListener).toHaveBeenCalledTimes(1);
    expect(h.timers).toHaveLength(0);
  });
});
