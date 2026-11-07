import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const stampDescriptor = Object.getOwnPropertyDescriptor(window, '__tsCreativeOrigin');
const SCRIPT_ORIGIN = 'https://ads.example.com:8443';
const STAMP_ORIGIN = 'https://publisher.example';

function setCurrentScript(src?: string, publicOrigin?: string): HTMLScriptElement | null {
  const script = src ? Object.assign(document.createElement('script'), { src }) : null;
  if (script && publicOrigin !== undefined) {
    script.setAttribute('data-ts-public-origin', publicOrigin);
  }
  Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
  return script;
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
  it('does not proxy a server-rewritten resource when bundle and configured origins differ', async () => {
    vi.stubGlobal('location', {
      href: 'https://www.publisher.example/article',
      origin: 'https://www.publisher.example',
    });
    setCurrentScript(
      'https://www.publisher.example/static/runtime.js',
      'https://ads.publisher.example:8443'
    );
    const { shouldProxyExternalUrl } =
      await import('../../../src/integrations/creative/proxy_sign');

    expect(
      shouldProxyExternalUrl(
        'https://ads.publisher.example:8443/first-party/proxy?tsurl=https%3A%2F%2Fimg.example%2Fpixel.gif&tstoken=signed'
      )
    ).toBe(false);
  });

  it.each([
    [`${SCRIPT_ORIGIN}/first-party/proxy?token=1`, true],
    ['https://configured.example:9443/first-party/proxy?token=1', true],
    ['https://configured.example:9443/first-party/proxy-extra', false],
    ['https://configured.example:9443/first-party/proxy/', false],
    ['https://configured.example:9443/first-party/Proxy', false],
    ['https://configured.example/first-party/proxy', false],
    ['https://configured.example:8443/first-party/proxy', false],
    ['http://configured.example:9443/first-party/proxy', false],
    ['https://foreign.example/first-party/proxy', false],
  ])('recognizes exact proxy paths on either captured origin: %s', async (value, expected) => {
    setCurrentScript(`${SCRIPT_ORIGIN}/static/runtime.js`, 'https://configured.example:9443');
    const { isFirstPartyProxyUrl, resolveFirstPartyPath } =
      await import('../../../src/integrations/creative/first_party');

    expect(isFirstPartyProxyUrl(value)).toBe(expected);
    expect(resolveFirstPartyPath('/first-party/sign')).toBe(`${SCRIPT_ORIGIN}/first-party/sign`);
    expect(resolveFirstPartyPath('/first-party/proxy-rebuild')).toBe(
      `${SCRIPT_ORIGIN}/first-party/proxy-rebuild`
    );
  });

  it.each([
    undefined,
    '',
    'not an origin',
    '/relative',
    'null',
    'data:text/plain,origin',
    'ftp://configured.example',
  ])('ignores missing or unusable configured metadata: %s', async (metadata) => {
    setCurrentScript(`${SCRIPT_ORIGIN}/static/runtime.js`, metadata);
    const { isFirstPartyProxyUrl, firstPartyOrigin } =
      await import('../../../src/integrations/creative/first_party');
    expect(firstPartyOrigin()).toBe(SCRIPT_ORIGIN);
    expect(isFirstPartyProxyUrl(`${SCRIPT_ORIGIN}/first-party/proxy`)).toBe(true);
    expect(isFirstPartyProxyUrl('https://configured.example/first-party/proxy')).toBe(false);
  });

  it.each([
    ['https://CONFIGURED.example:443/', 'https://configured.example'],
    ['http://CONFIGURED.example:80/', 'http://configured.example'],
  ])('normalizes configured default ports: %s', async (metadata, origin) => {
    setCurrentScript(`${SCRIPT_ORIGIN}/static/runtime.js`, metadata);
    const { isFirstPartyProxyUrl } = await import('../../../src/integrations/creative/first_party');
    expect(isFirstPartyProxyUrl(`${origin}/first-party/proxy`)).toBe(true);
    expect(isFirstPartyProxyUrl(`${origin}:9443/first-party/proxy`)).toBe(false);
  });

  it('reads the executing script once and pins metadata against later mutation and replacement', async () => {
    const script = setCurrentScript(
      `${SCRIPT_ORIGIN}/static/runtime.js`,
      'https://configured.example'
    )!;
    const getScript = vi.fn(() => script);
    Object.defineProperty(document, 'currentScript', { configurable: true, get: getScript });
    const { firstPartyOrigin, isFirstPartyProxyUrl } =
      await import('../../../src/integrations/creative/first_party');
    expect(getScript).toHaveBeenCalledTimes(1);

    script.setAttribute('data-ts-public-origin', 'https://changed.example');
    script.src = 'https://changed.example/runtime.js';
    setCurrentScript('https://replacement.example/runtime.js', 'https://replacement.example');
    expect(firstPartyOrigin()).toBe(SCRIPT_ORIGIN);
    expect(isFirstPartyProxyUrl('https://configured.example/first-party/proxy')).toBe(true);
    expect(isFirstPartyProxyUrl('https://changed.example/first-party/proxy')).toBe(false);
    expect(isFirstPartyProxyUrl('https://replacement.example/first-party/proxy')).toBe(false);
  });

  it('captures spoofed configured metadata before startup, without authenticating it', async () => {
    setCurrentScript(`${SCRIPT_ORIGIN}/static/runtime.js`, 'https://attacker.invalid');
    const { isFirstPartyProxyUrl, firstPartyOrigin } =
      await import('../../../src/integrations/creative/first_party');
    expect(isFirstPartyProxyUrl('https://attacker.invalid/first-party/proxy')).toBe(true);
    expect(firstPartyOrigin()).toBe(SCRIPT_ORIGIN);
  });

  it('pins script origin ahead of a different document and parent stamp', async () => {
    setCurrentScript(`${SCRIPT_ORIGIN}/static/tsjs=tsjs-unified.min.js?v=hash`);
    setStamp(STAMP_ORIGIN);
    const { firstPartyOrigin, resolveFirstPartyPath } =
      await import('../../../src/integrations/creative/first_party');

    expect(firstPartyOrigin()).toBe(SCRIPT_ORIGIN);
    expect(resolveFirstPartyPath('/first-party/sign')).toBe(`${SCRIPT_ORIGIN}/first-party/sign`);

    const fake = document.createElement('script');
    fake.src = 'https://changed.example/runtime.js';
    Object.defineProperty(document, 'currentScript', {
      configurable: true,
      get: () => fake,
    });
    setStamp('https://changed.example');
    vi.stubGlobal('location', {
      href: 'https://changed.example/',
      origin: 'https://changed.example',
    });
    expect(firstPartyOrigin()).toBe(SCRIPT_ORIGIN);
  });

  it('accepts a currentScript shadow installed before capture despite the parent stamp', async () => {
    vi.stubGlobal('location', { href: 'about:srcdoc', origin: 'null' });
    setStamp(STAMP_ORIGIN);
    const fake = document.createElement('script');
    fake.setAttribute('src', 'https://attacker.invalid/static/runtime.js');
    Object.defineProperty(document, 'currentScript', {
      configurable: true,
      get: () => fake,
    });
    const { firstPartyOrigin, resolveFirstPartyPath } =
      await import('../../../src/integrations/creative/first_party');
    const { TRUSTED_BASE_URL } = await import('../../../src/shared/origin');

    // The stamp still pins the legacy base, but does not authenticate endpoints.
    expect(TRUSTED_BASE_URL).toBe(STAMP_ORIGIN);
    expect(firstPartyOrigin()).toBe('https://attacker.invalid');
    expect(resolveFirstPartyPath('/first-party/proxy-rebuild')).toBe(
      'https://attacker.invalid/first-party/proxy-rebuild'
    );
    expect(resolveFirstPartyPath('/first-party/sign')).toBe(
      'https://attacker.invalid/first-party/sign'
    );

    // Removing the shadow after capture cannot repair the captured origin.
    setCurrentScript(`${SCRIPT_ORIGIN}/static/runtime.js`);
    expect(firstPartyOrigin()).toBe('https://attacker.invalid');
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
