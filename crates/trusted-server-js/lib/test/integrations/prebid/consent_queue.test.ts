import { describe, expect, it } from 'vitest';

import {
  classify,
  ConsentQueue,
  isKnownWithdrawal,
  readConsentCookies,
  requestBody,
  type ConsentSnapshot,
  type CookieView,
} from '../../../src/integrations/prebid/consent_queue';

const all = ['p1', 'p4', 'v755'];
const tcf = (tcString: string, granted = all, gdprApplies = true) => ({
  tcf: { gdprApplies, tcString, granted },
});
const gpp = (gppString: string, applicableSections = [7]) => ({
  gpp: { gppString, applicableSections },
});
const noCookies: CookieView = {};

describe('requestBody', () => {
  it('sends only the readings present', () => {
    expect(JSON.parse(requestBody(tcf('CPa.AA'))!)).toEqual({ tcString: 'CPa.AA' });
    expect(requestBody(tcf('', [], false)), 'out-of-scope TCF leaves the cookie').toBe(undefined);
    expect(JSON.parse(requestBody({ ...tcf('', [], false), ...gpp('DBA') })!)).toEqual({
      gppString: 'DBA',
      applicableSections: [7],
    });
    expect(JSON.parse(requestBody(gpp('DBABMA~1YNN', [7, 8]))!)).toEqual({
      gppString: 'DBABMA~1YNN',
      applicableSections: [7, 8],
    });
    expect(JSON.parse(requestBody({ ...tcf('CPa.AA'), ...gpp('DBA') })!)).toEqual({
      tcString: 'CPa.AA',
      gppString: 'DBA',
      applicableSections: [7],
    });
  });

  it('omits an unsendable TCF reading and sends nothing without readings', () => {
    expect(requestBody(tcf('bad string;'))).toBeUndefined();
    expect(requestBody(tcf(''))).toBeUndefined();
    expect(JSON.parse(requestBody({ ...tcf('bad;'), ...gpp('DBA') })!)).toEqual({
      gppString: 'DBA',
      applicableSections: [7],
    });
    expect(requestBody({})).toBeUndefined();
  });

  it('drops a GPP reading that is not cookie-safe', () => {
    expect(requestBody(gpp('DBA;evil'))).toBeUndefined();
    expect(requestBody(gpp('DBA', [0]))).toBeUndefined();
  });
});

describe('readConsentCookies', () => {
  it('reads both consent cookies and marks duplicates', () => {
    expect(readConsentCookies('a=1; euconsent-v2=CPa.AA; __gpp=DBA')).toEqual({
      euconsent: 'CPa.AA',
      gpp: 'DBA',
    });
    expect(readConsentCookies('euconsent-v2=CPa.AA; euconsent-v2=CPb.AA')).toEqual({
      euconsent: null,
    });
    expect(
      classify({ tcf: { gdprApplies: true, tcString: 'CPa.AA', granted: all } }, undefined, {
        euconsent: null,
      }),
      'a duplicated cookie is a mismatch'
    ).toBe('barrier');
  });
});

describe('classify', () => {
  it('identifies known withdrawals from Purposes 1 and 4', () => {
    expect(isKnownWithdrawal(tcf('CPa.AA', ['p4']))).toBe(true);
    expect(isKnownWithdrawal(tcf('CPa.AA', ['p1']))).toBe(true);
    expect(isKnownWithdrawal(tcf('CPa.AA'))).toBe(false);
    expect(isKnownWithdrawal(tcf('', [], false))).toBe(false);
    expect(isKnownWithdrawal(gpp('DBA'))).toBe(false);
    expect(classify(tcf('CPa.AA', ['p4']), undefined, { euconsent: 'CPa.AA' })).toBe('barrier');
  });

  it('treats a new reading the navigation already carried as a grant', () => {
    const cookies = { euconsent: 'CPa.AA', gpp: 'DBA' };
    expect(classify({ ...tcf('CPa.AA'), ...gpp('DBA') }, undefined, cookies)).toBe('grant');
    expect(classify(tcf('', [], false), undefined, noCookies)).toBe('grant');
  });

  it('makes a new reading the navigation could not carry a barrier', () => {
    expect(classify(tcf('CPa.AA'), undefined, { euconsent: 'CPold.AA' })).toBe('barrier');
    expect(classify(gpp('DBA'), undefined, noCookies), 'GPP kept outside cookies').toBe('barrier');
    const previous: ConsentSnapshot = tcf('CPa.AA');
    expect(
      classify({ ...tcf('CPa.AA'), ...gpp('DBA') }, previous, { gpp: 'DBA' }),
      'GPP arriving after TCF with a matching cookie'
    ).toBe('grant');
  });

  it('classifies changed TCF readings by lost grants', () => {
    const previous = tcf('CPa.AA', ['p1', 'p4', 'v755']);
    expect(classify(tcf('CPb.AA', ['p1', 'p4', 'v755', 'v8']), previous, noCookies)).toBe('grant');
    expect(classify(tcf('CPb.AA', ['p1', 'p4']), previous, noCookies), 'lost vendor').toBe(
      'barrier'
    );
    expect(classify(tcf('CPb.AA'), tcf('', [], false), noCookies), 'GDPR newly applies').toBe(
      'barrier'
    );
    expect(
      classify(tcf('', [], false), previous, noCookies),
      'GDPR ceasing to apply drops grants, so it stays conservative'
    ).toBe('barrier');
  });

  it('treats any changed GPP reading as a barrier', () => {
    expect(classify(gpp('DBB'), gpp('DBA'), noCookies)).toBe('barrier');
    expect(classify(gpp('DBA', [8]), gpp('DBA', [7]), noCookies)).toBe('barrier');
    expect(classify(gpp('DBA'), gpp('DBA'), noCookies)).toBe('grant');
  });
});

describe('ConsentQueue', () => {
  const cookies = { euconsent: 'CPa.AA' };

  it('sends one call at a time and ignores a repeat of the tail', () => {
    const queue = new ConsentQueue();
    expect(queue.offer(tcf('CPa.AA'), cookies)?.kind).toBe('grant');
    expect(queue.offer(tcf('CPa.AA'), cookies), 'should ignore an identical snapshot').toBe(
      undefined
    );
    const first = queue.next();
    expect(first?.body).toContain('CPa.AA');
    expect(queue.next(), 'should hold the next call while one is in flight').toBeUndefined();
    expect(queue.succeeded()).toBe(true);
    expect(queue.offer(tcf('CPa.AA'), cookies), 'should ignore the last sent snapshot').toBe(
      undefined
    );
  });

  it('lets a newer grant replace a waiting grant', () => {
    const queue = new ConsentQueue();
    queue.offer(tcf('CPa.AA'), cookies);
    queue.next();
    queue.offer(tcf('CPb.AA', [...all, 'v8']), cookies);
    queue.offer(tcf('CPc.AA', [...all, 'v8', 'v9']), cookies);
    expect(queue.succeeded(), 'should discard a result overtaken by a waiting change').toBe(false);
    expect(queue.next()?.body).toContain('CPc.AA');
    expect(queue.succeeded()).toBe(true);
    expect(queue.next()).toBeUndefined();
  });

  it('sends grant A, withdrawal B and grant C in order', () => {
    const queue = new ConsentQueue();
    queue.offer(tcf('CPa.AA'), cookies);
    const sent = [queue.next()!.body];
    expect(queue.offer(tcf('CPb.AA', ['v755']), cookies)?.kind).toBe('barrier');
    expect(queue.offer(tcf('CPc.AA'), cookies)?.kind).toBe('grant');
    expect(queue.succeeded()).toBe(false);
    sent.push(queue.next()!.body);
    expect(queue.succeeded()).toBe(false);
    sent.push(queue.next()!.body);
    expect(queue.succeeded()).toBe(true);
    expect(sent.map((body) => JSON.parse(body).tcString)).toEqual(['CPa.AA', 'CPb.AA', 'CPc.AA']);
  });

  it('sends a regrant equal to an earlier grant behind a withdrawal', () => {
    const queue = new ConsentQueue();
    queue.offer(tcf('CPa.AA'), cookies);
    queue.next();
    queue.offer(tcf('CPb.AA', ['v755']), cookies);
    expect(queue.offer(tcf('CPa.AA'), cookies), 'A, B, A must send three calls').toBeDefined();
  });

  it('never replaces a waiting barrier', () => {
    const queue = new ConsentQueue();
    queue.offer(tcf('CPa.AA'), cookies);
    queue.next();
    queue.offer(tcf('CPb.AA', ['p1']), cookies);
    queue.offer(tcf('CPc.AA'), cookies);
    queue.offer(tcf('CPd.AA', [...all, 'v1']), cookies);
    queue.succeeded();
    expect(JSON.parse(queue.next()!.body).tcString).toBe('CPb.AA');
    queue.succeeded();
    expect(JSON.parse(queue.next()!.body).tcString, 'grants behind it still coalesce').toBe(
      'CPd.AA'
    );
  });

  it('keeps a failed barrier at the head and drops a failed grant', () => {
    const queue = new ConsentQueue();
    queue.offer(tcf('CPa.AA', ['p1']), cookies);
    expect(queue.next()?.kind).toBe('barrier');
    queue.failed();
    queue.offer(tcf('CPb.AA'), cookies);
    expect(JSON.parse(queue.next()!.body).tcString, 'barrier resent first').toBe('CPa.AA');
    queue.succeeded();
    expect(JSON.parse(queue.next()!.body).tcString).toBe('CPb.AA');
    queue.failed();
    expect(queue.next(), 'a failed grant is not resent on a trigger').toBeUndefined();
    expect(
      queue.offer(tcf('CPb.AA'), cookies),
      'a new identical reading sends again'
    ).toBeDefined();
  });

  it('reports whether a call is pending', () => {
    const queue = new ConsentQueue();
    expect(queue.pending).toBe(false);
    queue.offer(tcf('CPa.AA'), cookies);
    expect(queue.pending).toBe(true);
    queue.next();
    queue.succeeded();
    expect(queue.pending).toBe(false);
  });
});
