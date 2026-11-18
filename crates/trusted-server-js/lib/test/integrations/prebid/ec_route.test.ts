import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { EcRouteClient, validEcRoute } from '../../../src/integrations/prebid/ec_route';

const mirrorName = '__Host-ts-eids-mirror';
const all = ['p1', 'p4', 'v755'];
const tcf = (tcString: string, granted = all) => ({
  tcf: { gdprApplies: true, tcString, granted },
});
const routeId = { source: 'route.example', uids: [{ id: 'route-id', atype: 1 }] };
const cookieId = { source: 'cookie.example', uids: [{ id: 'cookie-id', atype: 1 }] };

function setMirror(eids: unknown): void {
  document.cookie = `${mirrorName}=${Buffer.from(JSON.stringify({ v: 1, eids })).toString('base64url')}; Path=/; Secure`;
}
function clearCookies(): void {
  for (const name of [mirrorName, 'euconsent-v2', '__gpp']) {
    document.cookie = `${name}=; Path=/; Secure; Max-Age=0`;
  }
}

interface Pending {
  body: string;
  init: RequestInit;
  resolve: (response: Response) => void;
  reject: (error: unknown) => void;
}

function controlledFetch() {
  const calls: Pending[] = [];
  const fetch = vi.fn((_url: string, init: RequestInit) => {
    return new Promise<Response>((resolve, reject) => {
      calls.push({ body: init.body as string, init, resolve, reject });
    });
  });
  return { fetch, calls };
}

function json(eids: unknown[], status = 200): Response {
  return new Response(JSON.stringify({ v: 1, eids }), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

async function settle(): Promise<void> {
  for (let i = 0; i < 5; i++) await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function sentStrings(calls: Pending[]): string[] {
  return calls.map((call) => JSON.parse(call.body).tcString);
}

describe('validEcRoute', () => {
  it('accepts only same-origin paths', () => {
    expect(validEcRoute('/_ts/api/v1/ec', window.location)).toBe('/_ts/api/v1/ec');
    expect(validEcRoute('//evil.example/ec', window.location)).toBeUndefined();
    expect(validEcRoute('https://evil.example/ec', window.location)).toBeUndefined();
    expect(validEcRoute('relative/ec', window.location)).toBeUndefined();
    expect(validEcRoute('/\\evil.example', window.location)).toBeUndefined();
    expect(validEcRoute(42, window.location)).toBeUndefined();
    expect(validEcRoute(undefined, window.location)).toBeUndefined();
  });
});

describe('EcRouteClient', () => {
  let fetch: ReturnType<typeof controlledFetch>['fetch'];
  let calls: Pending[];
  let beforeSend: ReturnType<typeof vi.fn>;
  let client: EcRouteClient;

  beforeEach(() => {
    clearCookies();
    ({ fetch, calls } = controlledFetch());
    beforeSend = vi.fn();
    client = new EcRouteClient('/_ts/api/v1/ec', fetch, beforeSend);
    document.cookie = 'euconsent-v2=CPa.AA; Path=/; Secure';
  });
  afterEach(() => {
    clearCookies();
    vi.restoreAllMocks();
  });

  it('posts the snapshot with same-origin, uncached, keepalive options', () => {
    client.report(tcf('CPa.AA'));
    expect(beforeSend, 'should copy the opt-out before each call').toHaveBeenCalledOnce();
    expect(fetch).toHaveBeenCalledWith('/_ts/api/v1/ec', {
      method: 'POST',
      credentials: 'same-origin',
      cache: 'no-store',
      redirect: 'error',
      keepalive: true,
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ tcString: 'CPa.AA' }),
    });
  });

  it('merges an applied result while the mirror cookie is unchanged', async () => {
    setMirror([cookieId]);
    client.report(tcf('CPa.AA'));
    expect(client.mirrorSource(), 'a grant keeps the navigation mirror').toEqual([cookieId]);
    calls[0].resolve(json([routeId]));
    await settle();
    expect(client.mirrorSource()).toEqual([routeId]);

    setMirror([cookieId, routeId]);
    expect(client.mirrorSource(), 'a changed cookie supersedes the route result').toEqual([
      cookieId,
      routeId,
    ]);
  });

  it('treats 204 as no IDs and checks expiry at each merge', async () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    vi.setSystemTime(1_000_000_000);
    client.report(tcf('CPa.AA'));
    calls[0].resolve(json([{ source: 'short.example', uids: [{ id: 'short', exp: 1_000_010 }] }]));
    await settle();
    expect(client.mirrorSource()).toEqual([{ source: 'short.example', uids: [{ id: 'short' }] }]);
    vi.setSystemTime(1_000_020_000);
    expect(client.mirrorSource(), 'an expired route ID is dropped').toEqual([]);
    vi.useRealTimers();

    client.report(tcf('CPb.AA', [...all, 'v9']));
    calls[1].resolve(new Response(null, { status: 204 }));
    await settle();
    expect(client.mirrorSource()).toEqual([]);
  });

  it('withdraws while a grant is in flight: clears, suppresses and orders A, B, C', async () => {
    setMirror([cookieId]);
    client.report(tcf('CPa.AA'));
    client.report(tcf('CPb.AA', ['v755']));
    expect(document.cookie, 'a known withdrawal expires the mirror at once').not.toContain(
      mirrorName
    );
    client.report(tcf('CPc.AA'));
    expect(calls).toHaveLength(1);

    // Grant A's late answer re-sets the mirror; it must not reach an auction.
    setMirror([routeId]);
    calls[0].resolve(json([routeId]));
    await settle();
    expect(client.mirrorSource(), 'suppressed until the barrier is answered').toEqual([]);

    calls[1].resolve(new Response(null, { status: 204 }));
    await settle();
    expect(client.mirrorSource(), 'still suppressed while C waits').toEqual([]);
    calls[2].resolve(json([cookieId]));
    await settle();
    expect(sentStrings(calls)).toEqual(['CPa.AA', 'CPb.AA', 'CPc.AA']);
    expect(client.mirrorSource()).toEqual([cookieId]);
  });

  it('clears a stale result when a barrier is queued and when a result is discarded', async () => {
    client.report(tcf('CPa.AA'));
    calls[0].resolve(json([routeId]));
    await settle();
    expect(client.mirrorSource()).toEqual([routeId]);

    client.report({ ...tcf('CPa.AA'), gpp: { gppString: 'DBA', applicableSections: [7] } });
    expect(client.mirrorSource(), 'an unclassifiable GPP reading suppresses').toEqual([]);
    client.report({
      ...tcf('CPb.AA', [...all, 'v9']),
      gpp: { gppString: 'DBA', applicableSections: [7] },
    });
    calls[1].resolve(json([routeId]));
    await settle();
    calls[2].resolve(new Response(null, { status: 204 }));
    await settle();
    expect(client.mirrorSource(), 'the discarded answer never revives older IDs').toEqual([]);
  });

  it('keeps a failed barrier and resends it on the next trigger', async () => {
    client.report(tcf('CPa.AA', ['p1']));
    calls[0].resolve(new Response('nope', { status: 503 }));
    await settle();
    expect(calls).toHaveLength(1);
    client.pump();
    expect(calls).toHaveLength(2);
    expect(calls[1].body).toBe(calls[0].body);
    calls[1].reject(new TypeError('network'));
    await settle();
    client.pump();
    expect(calls).toHaveLength(3);
  });

  it('sends what waited behind a failed grant without another trigger', async () => {
    client.report(tcf('CPa.AA'));
    client.report(tcf('CPb.AA', ['v755']));
    calls[0].resolve(new Response('', { status: 500 }));
    await settle();
    expect(sentStrings(calls), 'the withdrawal must not stall').toEqual(['CPa.AA', 'CPb.AA']);

    calls[1].resolve(new Response(null, { status: 204 }));
    await settle();
    client.report(tcf('CPc.AA'));
    client.report(tcf('CPd.AA', [...all, 'v9']));
    calls[2].reject(new TypeError('network'));
    await settle();
    expect(sentStrings(calls), 'a waiting grant is sent too').toEqual([
      'CPa.AA',
      'CPb.AA',
      'CPc.AA',
      'CPd.AA',
    ]);
  });

  it('never serves an older result after a grant answer is overtaken', async () => {
    client.report(tcf('CPa.AA'));
    calls[0].resolve(json([routeId]));
    await settle();
    expect(client.mirrorSource()).toEqual([routeId]);

    client.report(tcf('CPb.AA', [...all, 'v8']));
    client.report(tcf('CPc.AA', [...all, 'v8', 'v9']));
    calls[1].resolve(json([routeId]));
    await settle();
    calls[2].resolve(new Response('', { status: 500 }));
    await settle();
    setMirror([cookieId]);
    expect(client.mirrorSource(), 'falls back to the mirror cookie').toEqual([cookieId]);
    document.cookie = `${mirrorName}=; Path=/; Secure; Max-Age=0`;
    expect(client.mirrorSource(), 'not the earlier route result').toEqual([]);
  });

  it('enforces the response cap in bytes, including a declared length', async () => {
    const answers = [
      () =>
        new Response('{}', { status: 200, headers: { 'Content-Length': String(16 * 1024 + 1) } }),
      // 6,000 three-byte characters: under 16 KiB in UTF-16 units, over it in bytes.
      () =>
        new Response(JSON.stringify({ v: 1, eids: [], pad: '\u20ac'.repeat(6000) }), {
          status: 200,
        }),
    ];
    for (const [index, answer] of answers.entries()) {
      const local = controlledFetch();
      const fresh = new EcRouteClient('/_ts/api/v1/ec', local.fetch, () => {});
      fresh.report(tcf('CPa.AA', ['p1']));
      local.calls[0].resolve(answer());
      await settle();
      fresh.pump();
      expect(local.calls, `answer ${index} must be a failure`).toHaveLength(2);
    }
  });

  it('drops a failed grant rather than replaying it on a trigger', async () => {
    client.report(tcf('CPa.AA'));
    calls[0].resolve(new Response('', { status: 500 }));
    await settle();
    client.pump();
    expect(calls).toHaveLength(1);
  });

  it('treats oversized, invalid and unexpected answers as failures', async () => {
    const answers = [
      () => new Response('x'.repeat(16 * 1024 + 1), { status: 200 }),
      () => new Response('{"v":2,"eids":[]}', { status: 200 }),
      () => new Response('not json', { status: 200 }),
      () => new Response('', { status: 201 }),
    ];
    for (const [index, answer] of answers.entries()) {
      const local = controlledFetch();
      const fresh = new EcRouteClient('/_ts/api/v1/ec', local.fetch, () => {});
      fresh.report(tcf('CPa.AA', ['p1']));
      local.calls[0].resolve(answer());
      await settle();
      expect(fresh.mirrorSource(), `answer ${index} must not lift suppression`).toEqual([]);
      fresh.pump();
      expect(local.calls, `answer ${index} must leave the barrier queued`).toHaveLength(2);
    }
  });

  it('contains a fetch that throws synchronously', () => {
    const throwing = new EcRouteClient(
      '/_ts/api/v1/ec',
      vi.fn(() => {
        throw new TypeError('blocked');
      }),
      () => {}
    );
    expect(() => throwing.report(tcf('CPa.AA', ['p1']))).not.toThrow();
  });

  it('keeps an unsendable withdrawal suppressed when an earlier grant answers', async () => {
    client.report(tcf('CPa.AA'));
    client.report(tcf('', []));
    calls[0].resolve(json([routeId]));
    await settle();
    setMirror([routeId]);
    expect(client.mirrorSource(), 'the late grant answer must not lift it').toEqual([]);

    client.report(tcf('CPb.AA'));
    calls[1].resolve(json([cookieId]));
    await settle();
    expect(client.mirrorSource(), 'a later valid reading lifts it').toEqual([cookieId]);
  });

  it('resends an earlier state after an unsendable withdrawal and lifts on its answer', async () => {
    client.report(tcf('CPa.AA'));
    calls[0].resolve(json([routeId]));
    await settle();
    client.report(tcf('', []));
    expect(client.mirrorSource()).toEqual([]);
    client.report(tcf('CPa.AA'));
    expect(calls, 'the repeat of A is sent again').toHaveLength(2);
    calls[1].resolve(json([routeId]));
    await settle();
    expect(client.mirrorSource()).toEqual([routeId]);
  });

  it('resends an unchanged reading after a post-fence grant fails', async () => {
    client.report(tcf('CPa.AA'));
    calls[0].resolve(json([routeId]));
    await settle();
    client.report(tcf('', []));
    client.report(tcf('CPa.AA'));
    calls[1].reject(new TypeError('network'));
    await settle();
    client.report(tcf('CPa.AA'));
    expect(calls, 'the unchanged reading is sent again').toHaveLength(3);
    calls[2].resolve(json([routeId]));
    await settle();
    expect(client.mirrorSource()).toEqual([routeId]);
  });

  it('sends no call for a withdrawal without its TC string, and stays suppressed', async () => {
    const gpp = { gppString: 'DBA', applicableSections: [7] };
    client.report({ ...tcf('CPa.AA'), gpp });
    calls[0].resolve(json([routeId]));
    await settle();
    for (let i = 0; i < 3; i++) client.report({ ...tcf('bad;', ['p4']), gpp });
    expect(calls, 'a GPP-only call would be answered from the stale cookie').toHaveLength(1);
    expect(client.mirrorSource()).toEqual([]);

    client.report({ ...tcf('CPb.AA'), gpp });
    calls[1].resolve(json([cookieId]));
    await settle();
    expect(client.mirrorSource(), 'a later valid reading lifts it').toEqual([cookieId]);
  });

  it('does not let a matching call queued before the withdrawal lift it', async () => {
    const gpp = { gppString: 'DBA', applicableSections: [7] };
    client.report({ gpp });
    client.report({ ...tcf('bad;', ['p4']), gpp });
    calls[0].resolve(json([routeId]));
    await settle();
    setMirror([routeId]);
    expect(client.mirrorSource()).toEqual([]);
  });

  it('lifts suppression after a repeated withdrawal is answered', async () => {
    client.report(tcf('CPa.AA', ['p1']));
    client.report(tcf('CPa.AA', ['p1']));
    expect(calls).toHaveLength(1);
    calls[0].resolve(json([routeId]));
    await settle();
    expect(client.mirrorSource()).toEqual([routeId]);
  });

  it('suppresses on a known withdrawal it cannot send', () => {
    setMirror([cookieId]);
    client.report(tcf('not valid;', ['p1']));
    expect(calls).toHaveLength(0);
    expect(client.mirrorSource()).toEqual([]);
  });
});
