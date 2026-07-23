import { afterEach, describe, expect, it, vi } from 'vitest';

const currentScriptDescriptor = Object.getOwnPropertyDescriptor(document, 'currentScript');

function setCurrentScript(src?: string): void {
  const script = src ? Object.assign(document.createElement('script'), { src }) : null;
  Object.defineProperty(document, 'currentScript', {
    configurable: true,
    value: script,
  });
}

afterEach(() => {
  vi.resetModules();
  if (currentScriptDescriptor) {
    Object.defineProperty(document, 'currentScript', currentScriptDescriptor);
  } else {
    Reflect.deleteProperty(document, 'currentScript');
  }
});

describe('creative/first_party.ts', () => {
  it('captures the injected classic script origin and resolves endpoints against it', async () => {
    setCurrentScript('https://ads.example.com:8443/static/tsjs=tsjs-unified.min.js?v=hash');
    const { firstPartyOrigin, resolveFirstPartyPath, isFirstPartyProxyUrl } =
      await import('../../../src/integrations/creative/first_party');

    expect(firstPartyOrigin()).toBe('https://ads.example.com:8443');
    expect(resolveFirstPartyPath('/first-party/sign')).toBe(
      'https://ads.example.com:8443/first-party/sign'
    );
    expect(isFirstPartyProxyUrl('https://ads.example.com:8443/first-party/proxy?token=1')).toBe(
      true
    );
    expect(isFirstPartyProxyUrl('https://ads.example.com:8443/first-party/proxy-extra')).toBe(
      false
    );
    expect(isFirstPartyProxyUrl('https://foreign.example/first-party/proxy')).toBe(false);
  });

  it('falls back to the document origin when currentScript is unavailable', async () => {
    setCurrentScript();
    const { firstPartyOrigin, resolveFirstPartyPath } =
      await import('../../../src/integrations/creative/first_party');

    expect(firstPartyOrigin()).toBe(location.origin);
    expect(resolveFirstPartyPath('/first-party/proxy-rebuild')).toBe(
      new URL('/first-party/proxy-rebuild', location.href).toString()
    );
  });
});
