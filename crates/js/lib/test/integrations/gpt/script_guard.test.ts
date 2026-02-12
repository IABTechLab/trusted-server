import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  installGptGuard,
  isGuardInstalled,
  resetGuardState,
} from '../../../src/integrations/gpt/script_guard';

describe('GPT script guard', () => {
  let originalDocumentWrite: typeof document.write;
  let originalDocumentWriteln: typeof document.writeln;
  let originalSetAttribute: typeof HTMLScriptElement.prototype.setAttribute;
  let originalCreateElement: typeof document.createElement;
  let originalAppendChild: typeof Element.prototype.appendChild;
  let originalInsertBefore: typeof Element.prototype.insertBefore;

  beforeEach(() => {
    originalDocumentWrite = document.write;
    originalDocumentWriteln = document.writeln;
    originalSetAttribute = HTMLScriptElement.prototype.setAttribute;
    originalCreateElement = document.createElement;
    originalAppendChild = Element.prototype.appendChild;
    originalInsertBefore = Element.prototype.insertBefore;
    resetGuardState();
  });

  afterEach(() => {
    resetGuardState();
    document.write = originalDocumentWrite;
    document.writeln = originalDocumentWriteln;
    HTMLScriptElement.prototype.setAttribute = originalSetAttribute;
    document.createElement = originalCreateElement;
    Element.prototype.appendChild = originalAppendChild;
    Element.prototype.insertBefore = originalInsertBefore;
  });

  it('restores patched globals on reset', () => {
    installGptGuard();
    expect(isGuardInstalled()).toBe(true);
    expect(document.write).not.toBe(originalDocumentWrite);
    expect(document.writeln).not.toBe(originalDocumentWriteln);
    expect(HTMLScriptElement.prototype.setAttribute).not.toBe(originalSetAttribute);
    expect(document.createElement).not.toBe(originalCreateElement);
    expect(Element.prototype.appendChild).not.toBe(originalAppendChild);
    expect(Element.prototype.insertBefore).not.toBe(originalInsertBefore);

    resetGuardState();

    expect(isGuardInstalled()).toBe(false);
    expect(document.write).toBe(originalDocumentWrite);
    expect(document.writeln).toBe(originalDocumentWriteln);
    expect(HTMLScriptElement.prototype.setAttribute).toBe(originalSetAttribute);
    expect(document.createElement).toBe(originalCreateElement);
    expect(Element.prototype.appendChild).toBe(originalAppendChild);
    expect(Element.prototype.insertBefore).toBe(originalInsertBefore);
  });

  it('reinstalls without recursive document.write calls', () => {
    installGptGuard();
    resetGuardState();
    installGptGuard();

    expect(() => {
      document.write('<script src="https://example.com/loader.js"></script>');
    }).not.toThrow();
  });

  it('rewrites through instance patch when src descriptor install is unavailable', () => {
    const nativeGetOwnPropertyDescriptor = Object.getOwnPropertyDescriptor;
    const descriptorSpy = vi.spyOn(Object, 'getOwnPropertyDescriptor').mockImplementation(
      (target: object, property: PropertyKey): PropertyDescriptor | undefined => {
        if (target === HTMLScriptElement.prototype && property === 'src') {
          return undefined;
        }
        return nativeGetOwnPropertyDescriptor(target, property);
      },
    );

    try {
      installGptGuard();

      const script = document.createElement('script');
      script.src =
        'https://securepubads.g.doubleclick.net/pagead/managed/js/gpt/current/pubads_impl.js';

      expect(script.getAttribute('src')).toContain(
        '/integrations/gpt/pagead/managed/js/gpt/current/pubads_impl.js',
      );
    } finally {
      descriptorSpy.mockRestore();
    }
  });

  it('does not rewrite URLs that only mention GPT domains in query text', () => {
    installGptGuard();

    const container = document.createElement('div');
    const script = document.createElement('script');
    const originalUrl =
      'https://cdn.example.com/loader.js?ref=securepubads.g.doubleclick.net/tag/js/gpt.js';

    script.src = originalUrl;
    container.appendChild(script);

    expect(script.src).toBe(originalUrl);
  });

  it('rewrites GPT URLs by hostname', () => {
    installGptGuard();

    const container = document.createElement('div');
    const script = document.createElement('script');

    script.src =
      'https://securepubads.g.doubleclick.net/pagead/managed/js/gpt/current/pubads_impl.js?foo=bar';
    container.appendChild(script);

    expect(script.src).toContain(window.location.host);
    expect(script.src).toContain(
      '/integrations/gpt/pagead/managed/js/gpt/current/pubads_impl.js?foo=bar',
    );
  });
});
