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
  clearMirror();
  delete page.__tsjs_prebid;
  delete page.googletag;
  vi.restoreAllMocks();
});
function options(): Record<string, unknown> {
  return original.mock.calls[original.mock.calls.length - 1][0]!;
}

describe('identity-only Prebid integration', () => {
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
