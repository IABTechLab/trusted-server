import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const page = window as unknown as Record<string, unknown>;
const mirrorName = '__Host-ts-eids-mirror';
const routeId = { source: 'route.example', uids: [{ id: 'route-id', atype: 1 }] };
const sharedId = { source: 'pubcid.org', uids: [{ id: 'shared-id', atype: 1 }] };
const granted = {
  gdprApplies: true,
  eventStatus: 'tcloaded',
  tcString: 'CPa.AA',
  purpose: { consents: { 1: true, 4: true } },
  vendor: { consents: { 755: true } },
};

type TcfCallback = (data: unknown, success: boolean) => void;

function setMirror(eids: unknown): void {
  document.cookie = `${mirrorName}=${Buffer.from(JSON.stringify({ v: 1, eids })).toString('base64url')}; Path=/; Secure`;
}
function cookieValue(name: string): string | undefined {
  return document.cookie
    .split('; ')
    .find((part) => part.startsWith(`${name}=`))
    ?.slice(name.length + 1);
}
function clearCookies(): void {
  for (const name of [
    mirrorName,
    'euconsent-v2',
    'ts-eids',
    '_pubcid_optout',
    '_ts_pubcid_optout_src',
  ]) {
    document.cookie = `${name}=; Path=/; Secure; Max-Age=0`;
  }
}

function mockPrebid() {
  return {
    registerBidAdapter: vi.fn(),
    requestBids: vi.fn((_options?: Record<string, unknown>) => Promise.resolve()),
    processQueue: vi.fn(),
    removeAdUnit: vi.fn(),
    onEvent: vi.fn(),
    setConfig: vi.fn(),
    getConfig: vi.fn((_key?: string): unknown => undefined),
    getUserIdsAsEids: vi.fn(() => []),
    adUnits: [] as unknown[],
    que: [],
  };
}

function tcfCmp() {
  const listeners: TcfCallback[] = [];
  const api = vi.fn((command: string, _version: number, callback: TcfCallback) => {
    if (command === 'addEventListener') listeners.push(callback);
  });
  return {
    api,
    emit: (data: Record<string, unknown>) =>
      listeners.forEach((listener, index) => listener({ ...data, listenerId: index }, true)),
  };
}

interface Pending {
  body: string;
  resolve: (response: Response) => void;
}

async function settle(): Promise<void> {
  for (let i = 0; i < 5; i++) await Promise.resolve();
  await vi.advanceTimersByTimeAsync(0);
}

let prebid: ReturnType<typeof mockPrebid>;
let original: ReturnType<typeof mockPrebid>['requestBids'];
let calls: Pending[];

async function install(config: Record<string, unknown>): Promise<void> {
  page.__tsjs_prebid = config;
  const module = await import('../../../src/integrations/prebid/index');
  module.installPrebidNpm();
}

function lastAuctionEids(): unknown {
  const options = original.mock.calls.at(-1)?.[0] as
    | { ortb2?: { user?: { ext?: { eids?: unknown } } } }
    | undefined;
  return options?.ortb2?.user?.ext?.eids;
}

beforeEach(() => {
  vi.resetModules();
  vi.useFakeTimers();
  clearCookies();
  localStorage.clear();
  delete page.__tsjsPrebidShimInstalled;
  delete page.__tcfapi;
  prebid = mockPrebid();
  original = prebid.requestBids;
  page.pbjs = prebid;
  page.googletag = {
    cmd: { push: (fn: () => void) => fn() },
    pubads: () => ({ refresh: vi.fn() }),
  };
  calls = [];
  vi.spyOn(window, 'fetch').mockImplementation(
    (_input: RequestInfo | URL, init?: RequestInit) =>
      new Promise<Response>((resolve) => calls.push({ body: init!.body as string, resolve }))
  );
});
afterEach(() => {
  window.dispatchEvent(new PageTransitionEvent('pagehide'));
  clearCookies();
  localStorage.clear();
  delete page.__tsjs_prebid;
  delete page.__tcfapi;
  delete page.googletag;
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe('EC route consent reporting through the Prebid integration', () => {
  it.each(['identity', 'auction'])(
    'makes no route call without an injected route in %s mode',
    async (mode) => {
      const cmp = tcfCmp();
      page.__tcfapi = cmp.api;
      await install({ mode });
      cmp.emit(granted);
      vi.advanceTimersByTime(3000);
      expect(cmp.api, 'should not even subscribe').not.toHaveBeenCalled();
      expect(window.fetch).not.toHaveBeenCalled();
    }
  );

  it('ignores a cross-origin route', async () => {
    const cmp = tcfCmp();
    page.__tcfapi = cmp.api;
    await install({ mode: 'identity', ecRoute: '//evil.example/ec' });
    cmp.emit(granted);
    expect(window.fetch).not.toHaveBeenCalled();
  });

  it('reports consent and merges the answer into a publisher auction', async () => {
    const cmp = tcfCmp();
    page.__tcfapi = cmp.api;
    await install({ mode: 'identity', ecRoute: '/_ts/api/v1/ec' });
    cmp.emit(granted);
    expect(calls.map((call) => JSON.parse(call.body))).toEqual([{ tcString: 'CPa.AA' }]);

    calls[0].resolve(Response.json({ v: 1, eids: [routeId] }));
    await settle();
    (window as unknown as { pbjs: { requestBids: (o: unknown) => unknown } }).pbjs.requestBids({});
    expect(lastAuctionEids()).toEqual([routeId]);
  });

  it('discovers a CMP that appears after install on the capture tick', async () => {
    await install({ mode: 'identity', ecRoute: '/_ts/api/v1/ec' });
    const cmp = tcfCmp();
    page.__tcfapi = cmp.api;
    vi.advanceTimersByTime(1000);
    cmp.emit(granted);
    expect(calls).toHaveLength(1);
  });

  it('keeps IDs out of Prebid after a withdrawal issued while a grant is in flight', async () => {
    const cmp = tcfCmp();
    page.__tcfapi = cmp.api;
    await install({ mode: 'identity', ecRoute: '/_ts/api/v1/ec' });
    cmp.emit(granted);
    cmp.emit({ ...granted, tcString: 'CPb.AA', purpose: { consents: { 1: false } } });

    // The grant's late answer re-sets the mirror before the withdrawal is answered.
    setMirror([routeId]);
    calls[0].resolve(Response.json({ v: 1, eids: [routeId] }));
    await settle();
    const wrapped = (window as unknown as { pbjs: { requestBids: (o: unknown) => unknown } }).pbjs;
    wrapped.requestBids({});
    expect(lastAuctionEids(), 'no ID while the withdrawal is pending').toBeUndefined();

    expect(calls.map((call) => JSON.parse(call.body).tcString)).toEqual(['CPa.AA', 'CPb.AA']);
    document.cookie = `${mirrorName}=; Path=/; Secure; Max-Age=0`;
    calls[1].resolve(new Response(null, { status: 204 }));
    await settle();
    wrapped.requestBids({});
    expect(lastAuctionEids(), 'no ID after the withdrawal is answered').toBeUndefined();
  });

  it('reports consent in auction mode without merging IDs client-side', async () => {
    const cmp = tcfCmp();
    page.__tcfapi = cmp.api;
    setMirror([routeId]);
    await install({ mode: 'auction', ecRoute: '/_ts/api/v1/ec' });
    cmp.emit(granted);
    expect(calls).toHaveLength(1);
    calls[0].resolve(Response.json({ v: 1, eids: [routeId] }));
    await settle();
    (window as unknown as { pbjs: { requestBids: (o: unknown) => unknown } }).pbjs.requestBids({
      adUnits: [],
    });
    expect(lastAuctionEids()).toBeUndefined();
  });
});

describe('SharedID opt-out through the Prebid integration', () => {
  it('copies a localStorage opt-out at install in auction mode', async () => {
    localStorage.setItem('_pubcid_optout', '1');
    await install({ mode: 'auction' });
    expect(cookieValue('_pubcid_optout')).toBe('1');
  });

  it('copies a later opt-out before the next auction-mode request', async () => {
    await install({ mode: 'auction' });
    expect(cookieValue('_pubcid_optout')).toBeUndefined();
    localStorage.setItem('_pubcid_optout', '1');
    (window as unknown as { pbjs: { requestBids: (o: unknown) => unknown } }).pbjs.requestBids({
      adUnits: [],
    });
    expect(cookieValue('_pubcid_optout')).toBe('1');
  });

  it('drops mirrored SharedID from identity-mode auctions while opted out', async () => {
    setMirror([sharedId, routeId]);
    await install({ mode: 'identity' });
    const wrapped = (window as unknown as { pbjs: { requestBids: (o: unknown) => unknown } }).pbjs;
    wrapped.requestBids({});
    expect(lastAuctionEids()).toEqual([sharedId, routeId]);
    localStorage.setItem('_pubcid_optout', '1');
    wrapped.requestBids({});
    expect(lastAuctionEids()).toEqual([routeId]);
  });
});
