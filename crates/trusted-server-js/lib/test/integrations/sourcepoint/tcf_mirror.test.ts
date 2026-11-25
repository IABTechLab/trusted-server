import { JSDOM } from 'jsdom';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { installSourcepointTcfMirror } from '../../../src/integrations/sourcepoint/tcf_mirror';

type Target = Parameters<typeof installSourcepointTcfMirror>[0];
type Api = NonNullable<Target['__tcfapi']>;
type Callback = Parameters<Api>[2];
type Decision = NonNullable<Parameters<Callback>[0]>;

const COOKIE = '__Host-ts-tcf';
const ATTRIBUTES = 'Path=/; Secure; SameSite=Lax';

function consentString(timestamp: number, storage: boolean, personalized = false): string {
  // Construct a fictional TCF v2 core segment with LastUpdated and purpose bits.
  const bytes = Buffer.alloc(32);
  bytes[0] = 2 << 2;
  for (let i = 0; i < 36; i++) {
    const bit = 42 + i;
    bytes[bit >> 3] |= (Math.floor(timestamp / 2 ** (35 - i)) % 2) << (7 - (bit % 8));
  }
  if (storage) bytes[19] = 0x80;
  if (personalized) bytes[19] |= 0x10;
  return bytes.toString('base64url');
}

function cmp() {
  let listener: Callback | undefined;
  const api = vi.fn<Api>((command, version, callback) => {
    if (command === 'addEventListener' && version === 2) listener = callback;
  });
  const emit = (value: string, storage: boolean, data: Partial<Decision> = {}, success = true) =>
    listener?.(
      {
        cmpStatus: 'loaded',
        eventStatus: 'tcloaded',
        gdprApplies: true,
        listenerId: 7,
        tcString: value,
        purpose: { consents: { 1: storage } },
        ...data,
      },
      success
    );
  return { api, emit };
}

const pages: JSDOM[] = [];
const disposers: Array<() => void> = [];

function page(url = 'https://publisher.example/') {
  const dom = new JSDOM('', { url });
  pages.push(dom);
  const target = dom.window as unknown as Target;
  return {
    target,
    install: () => disposers.push(installSourcepointTcfMirror(target)),
    cookie: () => target.document.cookie,
  };
}

afterEach(() => {
  for (const dispose of disposers.splice(0)) dispose();
  for (const dom of pages.splice(0)) dom.window.close();
});

describe('Sourcepoint TCF mirror', () => {
  const grant = consentString(100, true);
  const deny = consentString(101, false);

  it('mirrors a settled grant and then carries withdrawal as the original denial string', () => {
    const { target, install, cookie } = page();
    const { api, emit } = cmp();
    target.__tcfapi = api;
    install();

    emit(grant, true);
    expect(cookie()).toBe(`${COOKIE}=${grant}`);

    emit(deny, false, { eventStatus: 'useractioncomplete' });
    expect(cookie()).toBe(`${COOKIE}=${deny}`);
  });

  it('preserves the publisher cookie and discards a stale fallback when it appears', () => {
    const { target, install, cookie } = page();
    const { api, emit } = cmp();
    target.__tcfapi = api;
    install();
    emit(grant, true);

    target.document.cookie = `euconsent-v2=${deny}; ${ATTRIBUTES}`;
    emit(grant, true);

    expect(cookie()).toBe(`euconsent-v2=${deny}`);
  });

  it('does not let a stale or tied grant replace a newer withdrawal', () => {
    const { target, install, cookie } = page();
    const { api, emit } = cmp();
    target.__tcfapi = api;
    install();

    emit(deny, false);
    emit(grant, true);
    emit(consentString(101, true), true);

    expect(cookie()).toBe(`${COOKIE}=${deny}`);
    emit(consentString(102, true), true);
    expect(cookie()).toBe(`${COOKIE}=${consentString(102, true)}`);
  });

  it('carries a same-tick user action withdrawing a purpose other than storage', () => {
    const { target, install, cookie } = page();
    const { api, emit } = cmp();
    target.__tcfapi = api;
    install();
    const personalized = consentString(100, true, true);
    emit(personalized, true);

    emit(grant, true, { eventStatus: 'useractioncomplete' });
    emit(personalized, true);

    expect(cookie()).toBe(`${COOKIE}=${grant}`);
  });

  it('ignores pending and failed callbacks without inferring a choice', () => {
    const { target, install, cookie } = page();
    const { api, emit } = cmp();
    target.__tcfapi = api;
    install();

    emit(grant, true, { eventStatus: 'cmpuishown' });
    emit(grant, true, { cmpStatus: 'stub' });
    emit(grant, true, {}, false);
    emit(grant, true, { gdprApplies: undefined });

    expect(cookie()).toBe('');
  });

  it.each([
    { gdprApplies: false },
    { tcString: undefined },
    { tcString: 'C'.repeat(3601) },
    { tcString: `${grant}; Domain=example.com` },
  ])('clears only its own stale signal on an unusable settled decision: %j', (data) => {
    const { target, install, cookie } = page();
    const { api, emit } = cmp();
    target.__tcfapi = api;
    install();
    emit(grant, true);
    target.document.cookie = `__gpp=example-gpp; ${ATTRIBUTES}`;

    emit(grant, true, data);

    expect(cookie()).toBe('__gpp=example-gpp');
  });

  it('attaches when a CMP loads late and ignores callbacks from a replaced API', () => {
    const { target, install, cookie } = page();
    const first = cmp();
    const second = cmp();
    install();

    target.__tcfapi = first.api;
    target.document.dispatchEvent(new Event('DOMContentLoaded'));
    first.emit(grant, true);
    expect(cookie()).toBe(`${COOKIE}=${grant}`);

    target.__tcfapi = second.api;
    // A queued callback can reveal an asynchronously replaced API even
    // without another script-load or focus event.
    first.emit(consentString(102, true), true);
    second.emit(deny, false);
    first.emit(consentString(102, true), true);

    expect(first.api).toHaveBeenCalledWith('removeEventListener', 2, expect.any(Function), 7);
    expect(cookie()).toBe(`${COOKIE}=${deny}`);
  });

  it('cleans up a replaced stub that reports its ID after retirement', () => {
    const { target, install, cookie } = page();
    const stub = cmp();
    const live = cmp();
    target.__tcfapi = stub.api;
    install();
    target.__tcfapi = live.api;
    target.dispatchEvent(new Event('pageshow'));

    stub.emit(grant, true);

    expect(stub.api).toHaveBeenCalledWith('removeEventListener', 2, expect.any(Function), 7);
    expect(cookie()).toBe('');
    live.emit(grant, true);
    expect(cookie()).toBe(`${COOKIE}=${grant}`);
  });

  it('does not subscribe in an insecure page', () => {
    const { target, install } = page('http://publisher.example/');
    const { api } = cmp();
    target.__tcfapi = api;

    install();

    expect(api).not.toHaveBeenCalled();
  });
});
