import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const stampDescriptor = Object.getOwnPropertyDescriptor(window, '__tsCreativeOrigin');
const SCRIPT_ORIGIN = 'https://ads.example.com:8443';
const STAMP_ORIGIN = 'https://publisher.example';

function setCurrentScript(src?: string): void {
  const script = src ? Object.assign(document.createElement('script'), { src }) : null;
  Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
}

function setStamp(origin: string): void {
  Object.defineProperty(window, '__tsCreativeOrigin', { configurable: true, value: origin });
}

beforeEach(() => {
  vi.resetModules();
  vi.stubGlobal('document', document.implementation.createHTMLDocument('creative'));
  vi.stubGlobal('location', {
    href: 'https://foreign.example/creative',
    origin: 'https://foreign.example',
  });
  setCurrentScript();
});

afterEach(() => {
  vi.unstubAllGlobals();
  if (stampDescriptor) Object.defineProperty(window, '__tsCreativeOrigin', stampDescriptor);
  else Reflect.deleteProperty(window, '__tsCreativeOrigin');
});

describe('creative/first_party.ts', () => {
  it('pins script origin ahead of a different document and parent stamp', async () => {
    setCurrentScript(`${SCRIPT_ORIGIN}/static/tsjs=tsjs-unified.min.js?v=hash`);
    setStamp(STAMP_ORIGIN);
    const { firstPartyOrigin, resolveFirstPartyPath } =
      await import('../../../src/integrations/creative/first_party');

    expect(firstPartyOrigin()).toBe(SCRIPT_ORIGIN);
    expect(resolveFirstPartyPath('/first-party/sign')).toBe(`${SCRIPT_ORIGIN}/first-party/sign`);

    setCurrentScript('https://changed.example/runtime.js');
    setStamp('https://changed.example');
    vi.stubGlobal('location', {
      href: 'https://changed.example/',
      origin: 'https://changed.example',
    });
    expect(firstPartyOrigin()).toBe(SCRIPT_ORIGIN);
  });

  it.each([
    [`${SCRIPT_ORIGIN}/first-party/proxy?token=1`, true],
    ['/first-party/proxy?token=1', true],
    ['//ads.example.com:8443/first-party/proxy', true],
    [`${SCRIPT_ORIGIN}/first-party/proxy-extra`, false],
    [`${SCRIPT_ORIGIN}/first-party/proxy/`, false],
    [`${SCRIPT_ORIGIN}/first-party/Proxy`, false],
    ['https://ads.example.com/first-party/proxy', false],
    ['http://ads.example.com:8443/first-party/proxy', false],
    [`${STAMP_ORIGIN}/first-party/proxy`, false],
    ['https://foreign.example/first-party/proxy', false],
  ])('recognizes only the exact proxy on the endpoint origin: %s', async (value, expected) => {
    setCurrentScript(`${SCRIPT_ORIGIN}/static/runtime.js`);
    setStamp(STAMP_ORIGIN);
    const { isFirstPartyProxyUrl } = await import('../../../src/integrations/creative/first_party');
    expect(isFirstPartyProxyUrl(value)).toBe(expected);
  });

  it('captures an absolute script URL inside an opaque srcdoc document', async () => {
    vi.stubGlobal('location', { href: 'about:srcdoc', origin: 'null' });
    setCurrentScript(`${SCRIPT_ORIGIN}/static/tsjs=tsjs-unified.min.js?v=hash`);
    setStamp(STAMP_ORIGIN);
    const { firstPartyOrigin, resolveFirstPartyPath } =
      await import('../../../src/integrations/creative/first_party');

    expect(firstPartyOrigin()).toBe(SCRIPT_ORIGIN);
    expect(resolveFirstPartyPath('/first-party/proxy-rebuild')).toBe(
      `${SCRIPT_ORIGIN}/first-party/proxy-rebuild`
    );
  });

  it('falls back to the document origin when currentScript is unavailable', async () => {
    const { firstPartyOrigin, resolveFirstPartyPath } =
      await import('../../../src/integrations/creative/first_party');

    expect(firstPartyOrigin()).toBe(location.origin);
    expect(resolveFirstPartyPath('/first-party/proxy-rebuild')).toBe(
      'https://foreign.example/first-party/proxy-rebuild'
    );
  });

  it.each(['https://foreign.example/creative', 'about:srcdoc'])(
    'falls back to the pinned parent stamp without currentScript at %s',
    async (href) => {
      vi.stubGlobal('location', { href, origin: new URL(href).origin });
      setStamp(STAMP_ORIGIN);
      const { firstPartyOrigin, resolveFirstPartyPath } =
        await import('../../../src/integrations/creative/first_party');

      expect(firstPartyOrigin()).toBe(STAMP_ORIGIN);
      expect(resolveFirstPartyPath('/first-party/sign')).toBe(`${STAMP_ORIGIN}/first-party/sign`);
      setStamp('https://changed.example');
      expect(firstPartyOrigin()).toBe(STAMP_ORIGIN);
    }
  );

  it('uses the inherited base origin for unstamped srcdoc without currentScript', async () => {
    vi.stubGlobal('location', { href: 'about:srcdoc', origin: 'null' });
    Object.defineProperty(document, 'baseURI', {
      configurable: true,
      value: 'http://localhost:7676/article',
    });
    const { firstPartyOrigin, resolveFirstPartyPath } =
      await import('../../../src/integrations/creative/first_party');

    expect(firstPartyOrigin()).toBe('http://localhost:7676');
    expect(resolveFirstPartyPath('/first-party/sign')).toBe(
      'http://localhost:7676/first-party/sign'
    );
  });

  it('does not return the string null when no usable origin exists', async () => {
    vi.stubGlobal('location', { href: 'about:srcdoc', origin: 'null' });
    const { firstPartyOrigin, resolveFirstPartyPath, isFirstPartyProxyUrl } =
      await import('../../../src/integrations/creative/first_party');

    expect(firstPartyOrigin()).toBeNull();
    expect(resolveFirstPartyPath('/first-party/sign')).toBe('/first-party/sign');
    expect(isFirstPartyProxyUrl('/first-party/proxy')).toBe(false);
  });

  it('ignores an unusable script origin and falls back to the pinned stamp', async () => {
    setCurrentScript('data:text/javascript,void%200');
    setStamp(STAMP_ORIGIN);
    const { firstPartyOrigin } = await import('../../../src/integrations/creative/first_party');
    expect(firstPartyOrigin()).toBe(STAMP_ORIGIN);
  });
});
