import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const page = window as unknown as Record<string, unknown>;
const mirrorName = '__Host-ts-eids-mirror';
function setMirror(eids: unknown, v: unknown = 1): void {
  document.cookie = `${mirrorName}=${Buffer.from(JSON.stringify({ v, eids })).toString('base64url')}; Path=/; Secure`;
}
function clearMirror(): void {
  document.cookie = `${mirrorName}=; Path=/; Secure; Max-Age=0`;
}
const mirrored = [{ source: 'mirror.example', uids: [{ id: 'server-id', atype: 1 }] }];
function mockPrebid() {
  return {
    registerBidAdapter: vi.fn(),
    requestBids: vi.fn(function (this: unknown, _options?: Record<string, unknown>) {
      return Promise.resolve();
    }),
    processQueue: vi.fn(),
    removeAdUnit: vi.fn(),
    onEvent: vi.fn(),
    setConfig: vi.fn((_config?: Record<string, unknown>) => undefined),
    getConfig: vi.fn((_key?: string): unknown => undefined),
    getUserIdsAsEids: vi.fn(() => [
      { source: 'native.example', uids: [{ id: 'native-id', atype: 1 }] },
    ]),
    adUnits: [{ code: 'publisher-unit', bids: [{ bidder: 'publisher' }] }],
    que: [],
  };
}
let prebid: ReturnType<typeof mockPrebid>;
let original: ReturnType<typeof mockPrebid>['requestBids'];
let install: typeof import('../../../src/integrations/prebid/index').installPrebidNpm;
let refresh: ReturnType<typeof vi.fn>;
beforeEach(async () => {
  vi.resetModules();
  vi.useFakeTimers();
  clearMirror();
  document.cookie = 'ts-eids=; Path=/; Max-Age=0';
  delete page.__tsjsPrebidShimInstalled;
  prebid = mockPrebid();
  original = prebid.requestBids;
  page.pbjs = prebid;
  page.__tsjs_prebid = { mode: 'identity' };
  refresh = vi.fn();
  page.googletag = { cmd: { push: (fn: () => void) => fn() }, pubads: () => ({ refresh }) };
  install = (await import('../../../src/integrations/prebid/index')).installPrebidNpm;
});
afterEach(() => {
  window.dispatchEvent(new PageTransitionEvent('pagehide'));
  clearMirror();
  delete page.__tsjs_prebid;
  delete page.googletag;
  vi.restoreAllMocks();
  vi.useRealTimers();
});
function options(): Record<string, unknown> {
  return original.mock.calls[original.mock.calls.length - 1][0]!;
}

function capturedEids(): unknown {
  const cookie = document.cookie.split('; ').find((part) => part.startsWith('ts-eids='));
  return cookie ? JSON.parse(atob(cookie.slice('ts-eids='.length))) : undefined;
}

describe('identity-only Prebid integration', () => {
  it('captures available native IDs without starting an auction', () => {
    expect(capturedEids()).toEqual(prebid.getUserIdsAsEids());
    expect(original).not.toHaveBeenCalled();
    expect(prebid.processQueue).toHaveBeenCalledOnce();
  });
  it('captures delayed IDs and clears withdrawn IDs without any auction', () => {
    prebid.getUserIdsAsEids.mockReturnValue([]);
    vi.advanceTimersByTime(1000);
    expect(capturedEids()).toBeUndefined();
    const delayed = [{ source: 'delayed.example', uids: [{ id: 'later', atype: 1 }] }];
    prebid.getUserIdsAsEids.mockReturnValue(delayed);
    vi.advanceTimersByTime(1000);
    expect(capturedEids()).toEqual(delayed);
    prebid.getUserIdsAsEids.mockReturnValue([]);
    vi.advanceTimersByTime(1000);
    expect(capturedEids()).toBeUndefined();
    prebid.getUserIdsAsEids.mockReturnValue(delayed);
    vi.advanceTimersByTime(1000);
    expect(capturedEids()).toEqual(delayed);
    expect(original).not.toHaveBeenCalled();
  });

  it('does not rewrite unchanged cookies and restores a removed cookie', () => {
    const write = vi.spyOn(document, 'cookie', 'set');
    vi.advanceTimersByTime(3000);
    expect(write).not.toHaveBeenCalled();
    document.cookie = 'ts-eids=; Path=/; Secure; Max-Age=0';
    write.mockClear();
    vi.advanceTimersByTime(1000);
    expect(capturedEids()).toEqual(prebid.getUserIdsAsEids());
    expect(write).toHaveBeenCalledOnce();
  });

  it('clears stale capture on getter failure and recovers on the next sample', () => {
    const native = prebid.getUserIdsAsEids();
    prebid.getUserIdsAsEids.mockImplementation(() => {
      throw new Error('getter unavailable');
    });
    vi.advanceTimersByTime(1000);
    expect(capturedEids()).toBeUndefined();
    const write = vi.spyOn(document, 'cookie', 'set');
    vi.advanceTimersByTime(2000);
    expect(write).not.toHaveBeenCalled();
    prebid.getUserIdsAsEids.mockReturnValue(native);
    vi.advanceTimersByTime(1000);
    expect(capturedEids()).toEqual(native);
  });

  it('warns once per getter failure streak and retries after recovery', async () => {
    const { log } = await import('../../../src/core/log');
    const warn = vi.spyOn(log, 'warn');
    const native = prebid.getUserIdsAsEids();
    prebid.getUserIdsAsEids.mockImplementation(() => {
      throw new Error('unavailable');
    });
    vi.advanceTimersByTime(3000);
    expect(warn).toHaveBeenCalledOnce();
    prebid.getUserIdsAsEids.mockReturnValue(native);
    vi.advanceTimersByTime(1000);
    prebid.getUserIdsAsEids.mockImplementation(() => {
      throw new Error('unavailable again');
    });
    vi.advanceTimersByTime(3000);
    expect(warn).toHaveBeenCalledTimes(2);
    expect(capturedEids()).toBeUndefined();
  });

  it.each(['get', 'set'] as const)(
    'contains blocked cookie %s access during installation and publisher callbacks',
    async (accessor) => {
      const { log } = await import('../../../src/core/log');
      const warn = vi.spyOn(log, 'warn');
      window.dispatchEvent(new PageTransitionEvent('pagehide'));
      delete page.__tsjsPrebidShimInstalled;
      document.cookie = 'ts-eids=stale; Path=/; Secure';
      const blocked = vi.spyOn(document, 'cookie', accessor).mockImplementation(() => {
        throw new DOMException('Cookie access denied', 'SecurityError');
      });
      try {
        expect(() => install()).not.toThrow();
        const callback = vi.fn();
        prebid.requestBids({ bidsBackHandler: callback });
        expect(() => (options().bidsBackHandler as () => void)()).not.toThrow();
        expect(callback).toHaveBeenCalledOnce();
        expect(() => vi.advanceTimersByTime(3000)).not.toThrow();
        expect(warn).toHaveBeenCalledOnce();
      } finally {
        blocked.mockRestore();
      }
      vi.advanceTimersByTime(1000);
      expect(capturedEids()).toEqual(prebid.getUserIdsAsEids());
    }
  );

  it('captures the publisher queue result before any timer runs', () => {
    window.dispatchEvent(new PageTransitionEvent('pagehide'));
    delete page.__tsjsPrebidShimInstalled;
    prebid.processQueue.mockImplementation(() => {
      prebid.getUserIdsAsEids.mockReturnValue([
        { source: 'queued.example', uids: [{ id: 'queued-id', atype: 1 }] },
      ]);
    });
    install();
    expect(capturedEids()).toEqual(prebid.getUserIdsAsEids());
    expect(original).not.toHaveBeenCalled();
  });

  it('clears stale capture for a missing or invalid getter and accepts a late module', () => {
    const getter = prebid.getUserIdsAsEids;
    delete (prebid as Partial<typeof prebid>).getUserIdsAsEids;
    vi.advanceTimersByTime(1000);
    expect(capturedEids()).toBeUndefined();
    prebid.getUserIdsAsEids = getter;
    getter.mockReturnValue({ invalid: true } as unknown as ReturnType<typeof getter>);
    vi.advanceTimersByTime(1000);
    expect(capturedEids()).toBeUndefined();
    getter.mockReturnValue([{ source: 'late.example', uids: [{ id: 'ready', atype: 1 }] }]);
    vi.advanceTimersByTime(1000);
    expect(capturedEids()).toEqual(getter());
  });

  it('reads fresh consent-filtered IDs on visibility changes', () => {
    prebid.getUserIdsAsEids.mockReturnValue([]);
    document.dispatchEvent(new Event('visibilitychange'));
    expect(capturedEids()).toBeUndefined();
    expect(original).not.toHaveBeenCalled();
  });

  it('pauses in BFCache and resumes once without duplicating installation', () => {
    const getter = prebid.getUserIdsAsEids;
    window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true }));
    getter.mockClear();
    vi.advanceTimersByTime(3000);
    expect(getter).not.toHaveBeenCalled();
    window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
    window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
    install();
    getter.mockClear();
    vi.advanceTimersByTime(1000);
    expect(getter).toHaveBeenCalledOnce();
  });

  it('removes lifecycle listeners and stops sampling after permanent navigation', () => {
    window.dispatchEvent(new PageTransitionEvent('pagehide'));
    prebid.getUserIdsAsEids.mockClear();
    window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
    document.dispatchEvent(new Event('visibilitychange'));
    vi.advanceTimersByTime(3000);
    expect(prebid.getUserIdsAsEids).not.toHaveBeenCalled();
  });

  it('leaves publisher bidders, ad units, GPT and auction hooks untouched and installs once', () => {
    const remove = prebid.removeAdUnit;
    const wrapped = prebid.requestBids;
    install();
    expect(prebid.requestBids).toBe(wrapped);
    expect(prebid.registerBidAdapter).not.toHaveBeenCalled();
    expect(prebid.onEvent).not.toHaveBeenCalled();
    expect(prebid.removeAdUnit).toBe(remove);
    expect((page.googletag as { pubads(): { refresh: unknown } }).pubads().refresh).toBe(refresh);
    expect(prebid.adUnits[0].bids).toEqual([{ bidder: 'publisher' }]);
    expect(prebid.setConfig).not.toHaveBeenCalledWith(
      expect.objectContaining({ bidderTimeout: expect.anything() })
    );
  });
  it('retains managed User ID ownership and waits for publisher consent setup', () => {
    delete page.__tsjsPrebidShimInstalled;
    prebid.requestBids = original;
    const setConfig = prebid.setConfig;
    const managed = { name: 'sharedId', params: { create: false } };
    page.__tsjs_prebid = { mode: 'identity', managedUserIds: [managed] };
    install();
    expect(setConfig).not.toHaveBeenCalledWith(
      expect.objectContaining({ userSync: { userIds: [managed] } })
    );
    const consent = { gdpr: { cmpApi: 'static', consentData: { gdprApplies: false } } };
    prebid.getConfig.mockImplementation((key) =>
      key === 'consentManagement' ? consent : undefined
    );
    prebid.setConfig({ consentManagement: consent });
    prebid.setConfig({ userSync: { userIds: [{ name: 'sharedId', params: { create: true } }] } });
    expect(setConfig).toHaveBeenLastCalledWith({ userSync: { userIds: [managed] } });
    expect(prebid.registerBidAdapter).not.toHaveBeenCalled();
  });

  it('waits for a late CMP terminal result and retries a replaced silent CMP', () => {
    delete page.__tsjsPrebidShimInstalled;
    prebid.requestBids = original;
    page.__tsjs_prebid = { mode: 'identity', managedUserIds: [{ name: 'sharedId' }] };
    delete page.__tcfapi;
    const setConfig = prebid.setConfig;
    install();
    const first = vi.fn();
    page.__tcfapi = first;
    expect(first).toHaveBeenCalledWith('addEventListener', 2, expect.any(Function));
    expect(setConfig).not.toHaveBeenCalledWith(
      expect.objectContaining({ userSync: expect.objectContaining({ userIds: expect.anything() }) })
    );
    let answer: ((result: unknown, success: boolean) => void) | undefined;
    page.__tcfapi = vi.fn((command, _version, callback) => {
      if (command === 'addEventListener') answer = callback;
    });
    prebid.requestBids();
    expect(answer).toBeTypeOf('function');
    answer!({ eventStatus: 'cmpuishown', gdprApplies: true, listenerId: 2 }, true);
    expect(setConfig).not.toHaveBeenCalledWith(
      expect.objectContaining({ userSync: expect.objectContaining({ userIds: expect.anything() }) })
    );
    answer!(
      {
        eventStatus: 'useractioncomplete',
        gdprApplies: true,
        tcString: 'CPexampleConsent',
        listenerId: 2,
      },
      true
    );
    expect(setConfig).toHaveBeenCalledWith({ userSync: { userIds: [{ name: 'sharedId' }] } });
    expect(prebid.registerBidAdapter).not.toHaveBeenCalled();
    delete page.__tcfapi;
  });

  it('merges a UTF-8 mirror locally while preserving options, metadata and input objects', () => {
    const eids = [
      {
        source: 'mirror.example',
        inserter: 'example',
        ext: { source: true },
        uids: [
          { id: '識別子', atype: 1, exp: Math.floor(Date.now() / 1000) + 60, ext: { uid: true } },
        ],
      },
    ];
    setMirror(eids);
    const request = {
      timeout: 500,
      custom: true,
      ortb2: {
        site: { name: 'publisher' },
        user: {
          data: [],
          ext: {
            other: true,
            eids: [{ source: 'publisher.example', uids: [{ id: 'publisher-id' }] }],
          },
        },
      },
    };
    const before = structuredClone(request);
    prebid.requestBids(request);
    expect(options()).toMatchObject({
      timeout: 500,
      custom: true,
      ortb2: {
        site: { name: 'publisher' },
        user: {
          data: [],
          ext: {
            other: true,
            eids: [
              before.ortb2.user.ext.eids[0],
              { ...eids[0], uids: [{ id: '識別子', atype: 1, ext: { uid: true } }] },
            ],
          },
        },
      },
    });
    expect(request).toEqual(before);
    expect(prebid.setConfig).not.toHaveBeenCalledWith(
      expect.objectContaining({ ortb2: expect.anything() })
    );
  });
  it('deduplicates only mirror additions against both global and request EID locations', () => {
    const uid = (id: string, atype = 1) => ({ id, atype });
    prebid.getConfig.mockReturnValue({
      user: {
        eids: [{ source: 'mirror.example', uids: [uid('global-direct')] }],
        ext: { eids: [{ source: 'mirror.example', uids: [uid('global-ext')] }] },
      },
    });
    setMirror([
      {
        source: 'mirror.example',
        ext: { mirror: true },
        uids: [
          uid('global-direct'),
          uid('global-ext'),
          uid('request-direct'),
          uid('request-ext'),
          uid('new'),
          uid('new'),
          uid('new', 2),
        ],
      },
    ]);
    const publisher = {
      source: 'mirror.example',
      ext: { publisher: true },
      uids: [uid('request-ext'), uid('request-ext')],
    };
    const direct = [{ source: 'mirror.example', uids: [uid('request-direct')] }];
    prebid.requestBids({ ortb2: { user: { eids: direct, ext: { eids: [publisher] } } } });
    expect(options()).toMatchObject({
      ortb2: {
        user: {
          eids: direct,
          ext: {
            eids: [
              publisher,
              {
                source: 'mirror.example',
                ext: { mirror: true },
                uids: [uid('new'), uid('new', 2)],
              },
            ],
          },
        },
      },
    });
  });
  it('re-reads replacement, removal and expiry on each call', () => {
    vi.spyOn(Date, 'now').mockReturnValue(1_000_000);
    setMirror([
      {
        source: 'mirror.example',
        uids: [
          { id: 'first', exp: 1001 },
          { id: 'expired', exp: 1000 },
          { id: 'invalid', exp: '1001' },
          { id: 'missing' },
        ],
      },
    ]);
    prebid.requestBids();
    expect(options()).toMatchObject({
      ortb2: {
        user: {
          ext: { eids: [{ source: 'mirror.example', uids: [{ id: 'first' }, { id: 'missing' }] }] },
        },
      },
    });
    vi.mocked(Date.now).mockReturnValue(1_002_000);
    prebid.requestBids();
    expect(options()).toMatchObject({
      ortb2: { user: { ext: { eids: [{ source: 'mirror.example', uids: [{ id: 'missing' }] }] } } },
    });
    setMirror(mirrored);
    prebid.requestBids();
    expect(options()).toMatchObject({ ortb2: { user: { ext: { eids: mirrored } } } });
    clearMirror();
    prebid.requestBids();
    expect(options().ortb2).toBeUndefined();
  });
  it.each([
    'not_base64!',
    Buffer.from('{bad json').toString('base64url'),
    Buffer.from([0xff]).toString('base64url'),
    Buffer.from(JSON.stringify({ v: 2, eids: mirrored })).toString('base64url'),
    Buffer.from(
      JSON.stringify({
        v: 1,
        eids: [
          { source: '', uids: [{ id: 'a' }] },
          {
            source: 'mirror.example',
            uids: [{ id: '' }, { id: 42 }, { id: 'a', atype: '1' }, { id: 'a', exp: null }],
          },
        ],
      })
    ).toString('base64url'),
    'a'.repeat(3585),
  ])('ignores malformed, invalid or oversized mirrors (%#)', (cookie) => {
    document.cookie = `${mirrorName}=${cookie}; Path=/; Secure`;
    prebid.requestBids();
    expect(options().ortb2).toBeUndefined();
  });
  it('omits fractional expiry and isolates failures reading publisher global config', () => {
    setMirror([
      {
        source: 'mirror.example',
        uids: [{ id: 'fractional', exp: Math.floor(Date.now() / 1000) + 60.5 }],
      },
    ]);
    prebid.requestBids();
    expect(options().ortb2).toBeUndefined();
    setMirror(mirrored);
    prebid.getConfig.mockImplementation(() => {
      throw new Error('config unavailable');
    });
    expect(() => prebid.requestBids()).not.toThrow();
  });
  it('captures native IDs before the callback, preserving receiver, arguments and callback return', () => {
    setMirror(mirrored);
    const receiver = {};
    const callback = vi.fn(function (this: unknown, ...args: unknown[]) {
      expect(this).toBe(receiver);
      expect(args).toEqual([{ bids: true }, false, 'auction-id']);
      const payload = document.cookie
        .split('; ')
        .find((part) => part.startsWith('ts-eids='))!
        .slice('ts-eids='.length);
      expect(JSON.parse(atob(payload))).toEqual(prebid.getUserIdsAsEids());
      return 'callback-result';
    });
    prebid.requestBids({ bidsBackHandler: callback });
    expect(
      (options().bidsBackHandler as (...args: unknown[]) => unknown).call(
        receiver,
        { bids: true },
        false,
        'auction-id'
      )
    ).toBe('callback-result');
  });
  it('preserves the original promise, request receiver, omitted option defaults and errors', () => {
    const promise = Promise.resolve();
    original.mockReturnValue(promise);
    const receiver = {};
    expect(prebid.requestBids.call(receiver)).toBe(promise);
    expect(original.mock.contexts[original.mock.contexts.length - 1]).toBe(receiver);
    expect(options()).not.toHaveProperty('adUnits');
    const failure = new Error('publisher callback failure');
    prebid.requestBids({
      bidsBackHandler: () => {
        throw failure;
      },
    });
    expect(() => (options().bidsBackHandler as () => void)()).toThrow(failure);
    original.mockImplementation(() => {
      throw failure;
    });
    expect(() => prebid.requestBids()).toThrow(failure);
  });
});
