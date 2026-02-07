import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import {
  installDataDomeGuard,
  isGuardInstalled,
  resetGuardState,
  isDataDomeSdkUrl,
  extractDataDomePath,
  rewriteDataDomeUrl,
} from '../../../src/integrations/datadome/script_guard';

describe('DataDome SDK Script Interception Guard', () => {
  let originalAppendChild: typeof Element.prototype.appendChild;
  let originalInsertBefore: typeof Element.prototype.insertBefore;

  beforeEach(() => {
    // Store original methods
    originalAppendChild = Element.prototype.appendChild;
    originalInsertBefore = Element.prototype.insertBefore;

    // Reset guard state before each test
    resetGuardState();
  });

  afterEach(() => {
    // Restore original methods
    Element.prototype.appendChild = originalAppendChild;
    Element.prototype.insertBefore = originalInsertBefore;

    // Reset guard state after each test
    resetGuardState();
  });

  describe('isDataDomeSdkUrl', () => {
    it('should detect js.datadome.co URLs', () => {
      expect(isDataDomeSdkUrl('https://js.datadome.co/tags.js')).toBe(true);
      expect(isDataDomeSdkUrl('https://js.datadome.co/js/check')).toBe(true);
      expect(isDataDomeSdkUrl('//js.datadome.co/tags.js')).toBe(true);
      expect(isDataDomeSdkUrl('http://js.datadome.co/tags.js')).toBe(true);
    });

    it('should be case-insensitive', () => {
      expect(isDataDomeSdkUrl('https://JS.DATADOME.CO/tags.js')).toBe(true);
      expect(isDataDomeSdkUrl('https://Js.DataDome.Co/tags.js')).toBe(true);
    });

    it('should not match other datadome subdomains', () => {
      expect(isDataDomeSdkUrl('https://api.datadome.co/check')).toBe(false);
      expect(isDataDomeSdkUrl('https://datadome.co/tags.js')).toBe(false);
    });

    it('should not match non-datadome URLs', () => {
      expect(isDataDomeSdkUrl('https://example.com/tags.js')).toBe(false);
      expect(isDataDomeSdkUrl('https://cdn.example.com/js.datadome.co.js')).toBe(false);
    });

    it('should handle empty and null values', () => {
      expect(isDataDomeSdkUrl('')).toBe(false);
      expect(isDataDomeSdkUrl(null as unknown as string)).toBe(false);
      expect(isDataDomeSdkUrl(undefined as unknown as string)).toBe(false);
    });
  });

  describe('extractDataDomePath', () => {
    it('should extract path from absolute URLs', () => {
      expect(extractDataDomePath('https://js.datadome.co/tags.js')).toBe('/tags.js');
      expect(extractDataDomePath('https://js.datadome.co/js/check')).toBe('/js/check');
      expect(extractDataDomePath('http://js.datadome.co/js/foo/bar')).toBe('/js/foo/bar');
    });

    it('should extract path from protocol-relative URLs', () => {
      expect(extractDataDomePath('//js.datadome.co/tags.js')).toBe('/tags.js');
      expect(extractDataDomePath('//js.datadome.co/js/check')).toBe('/js/check');
    });

    it('should preserve query strings', () => {
      expect(extractDataDomePath('https://js.datadome.co/tags.js?key=abc')).toBe(
        '/tags.js?key=abc'
      );
      expect(extractDataDomePath('https://js.datadome.co/js/check?foo=bar&baz=qux')).toBe(
        '/js/check?foo=bar&baz=qux'
      );
    });

    it('should handle bare domain', () => {
      expect(extractDataDomePath('https://js.datadome.co')).toBe('/');
      expect(extractDataDomePath('https://js.datadome.co/')).toBe('/');
    });
  });

  describe('rewriteDataDomeUrl', () => {
    it('should rewrite to first-party URL with path preserved', () => {
      const rewritten = rewriteDataDomeUrl('https://js.datadome.co/tags.js');
      expect(rewritten).toContain('/integrations/datadome/tags.js');
      expect(rewritten).toContain(window.location.host);
    });

    it('should preserve the js/ path', () => {
      const rewritten = rewriteDataDomeUrl('https://js.datadome.co/js/check');
      expect(rewritten).toContain('/integrations/datadome/js/check');
    });

    it('should preserve query strings', () => {
      const rewritten = rewriteDataDomeUrl('https://js.datadome.co/tags.js?key=abc');
      expect(rewritten).toContain('/integrations/datadome/tags.js?key=abc');
    });
  });

  describe('installDataDomeGuard', () => {
    it('should install the guard successfully', () => {
      expect(isGuardInstalled()).toBe(false);

      installDataDomeGuard();

      expect(isGuardInstalled()).toBe(true);
    });

    it('should not install twice', () => {
      installDataDomeGuard();
      const firstInstall = Element.prototype.appendChild;

      installDataDomeGuard();
      const secondInstall = Element.prototype.appendChild;

      // Should be the same reference (no double patching)
      expect(firstInstall).toBe(secondInstall);
    });

    it('should patch Element.prototype.appendChild', () => {
      installDataDomeGuard();

      expect(Element.prototype.appendChild).not.toBe(originalAppendChild);
    });

    it('should patch Element.prototype.insertBefore', () => {
      installDataDomeGuard();

      expect(Element.prototype.insertBefore).not.toBe(originalInsertBefore);
    });
  });

  describe('appendChild interception', () => {
    it('should rewrite DataDome SDK URL', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://js.datadome.co/tags.js';

      container.appendChild(script);

      expect(script.src).toContain('/integrations/datadome/tags.js');
      expect(script.src).not.toContain('js.datadome.co');
    });

    it('should preserve path when rewriting', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://js.datadome.co/js/check';

      container.appendChild(script);

      expect(script.src).toContain('/integrations/datadome/js/check');
    });

    it('should use location.host for rewritten URL', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://js.datadome.co/tags.js';

      container.appendChild(script);

      expect(script.src).toContain(window.location.host);
      expect(script.src).toMatch(/^https?:\/\//);
    });

    it('should not rewrite non-DataDome scripts', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://example.com/some-script.js';

      container.appendChild(script);

      expect(script.src).toBe('https://example.com/some-script.js');
    });

    it('should handle scripts with setAttribute', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.setAttribute('src', 'https://js.datadome.co/tags.js');

      container.appendChild(script);

      expect(script.getAttribute('src')).toContain('/integrations/datadome/tags.js');
    });

    it('should not affect non-script elements', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const img = document.createElement('img');
      img.src = 'https://js.datadome.co/image.png';

      container.appendChild(img);

      expect(img.src).toBe('https://js.datadome.co/image.png');
    });

    it('should preserve other script attributes', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.setAttribute('async', '');
      script.setAttribute('crossorigin', 'anonymous');
      script.setAttribute('id', 'datadome-sdk');
      script.src = 'https://js.datadome.co/tags.js';

      container.appendChild(script);

      expect(script.getAttribute('async')).toBe('');
      expect(script.getAttribute('crossorigin')).toBe('anonymous');
      expect(script.getAttribute('id')).toBe('datadome-sdk');
    });
  });

  describe('insertBefore interception', () => {
    it('should rewrite DataDome SDK URL', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const reference = document.createElement('div');
      container.appendChild(reference);

      const script = document.createElement('script');
      script.src = 'https://js.datadome.co/tags.js';

      container.insertBefore(script, reference);

      expect(script.src).toContain('/integrations/datadome/tags.js');
      expect(script.src).not.toContain('js.datadome.co');
    });

    it('should not rewrite non-DataDome scripts', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const reference = document.createElement('div');
      container.appendChild(reference);

      const script = document.createElement('script');
      script.src = 'https://example.com/some-script.js';

      container.insertBefore(script, reference);

      expect(script.src).toBe('https://example.com/some-script.js');
    });

    it('should work with null reference node', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://js.datadome.co/tags.js';

      container.insertBefore(script, null);

      expect(script.src).toContain('/integrations/datadome/tags.js');
    });
  });

  describe('link preload interception', () => {
    it('should rewrite DataDome SDK preload link', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.href = 'https://js.datadome.co/tags.js';

      container.appendChild(link);

      expect(link.href).toContain('/integrations/datadome/tags.js');
      expect(link.href).not.toContain('js.datadome.co');
    });

    it('should use location.host for rewritten preload URL', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.href = 'https://js.datadome.co/tags.js';

      container.appendChild(link);

      expect(link.href).toContain(window.location.host);
      expect(link.href).toMatch(/^https?:\/\//);
    });

    it('should not rewrite preload links without as="script"', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'style');
      link.href = 'https://js.datadome.co/tags.js';

      container.appendChild(link);

      expect(link.href).toBe('https://js.datadome.co/tags.js');
    });

    it('should not rewrite links without rel="preload"', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'stylesheet');
      link.setAttribute('as', 'script');
      link.href = 'https://js.datadome.co/tags.js';

      container.appendChild(link);

      expect(link.href).toBe('https://js.datadome.co/tags.js');
    });

    it('should not rewrite non-DataDome preload links', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.href = 'https://example.com/other-script.js';

      container.appendChild(link);

      expect(link.href).toBe('https://example.com/other-script.js');
    });

    it('should work with insertBefore for preload links', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const reference = document.createElement('div');
      container.appendChild(reference);

      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.href = 'https://js.datadome.co/tags.js';

      container.insertBefore(link, reference);

      expect(link.href).toContain('/integrations/datadome/tags.js');
    });

    it('should handle preload link with setAttribute', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.setAttribute('href', 'https://js.datadome.co/tags.js');

      container.appendChild(link);

      expect(link.getAttribute('href')).toContain('/integrations/datadome/tags.js');
    });

    it('should preserve other link attributes', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.setAttribute('crossorigin', 'anonymous');
      link.setAttribute('id', 'datadome-preload');
      link.href = 'https://js.datadome.co/tags.js';

      container.appendChild(link);

      expect(link.getAttribute('rel')).toBe('preload');
      expect(link.getAttribute('as')).toBe('script');
      expect(link.getAttribute('crossorigin')).toBe('anonymous');
      expect(link.getAttribute('id')).toBe('datadome-preload');
    });
  });

  describe('link prefetch interception', () => {
    it('should rewrite DataDome SDK prefetch link', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'prefetch');
      link.setAttribute('as', 'script');
      link.href = 'https://js.datadome.co/tags.js';

      container.appendChild(link);

      expect(link.href).toContain('/integrations/datadome/tags.js');
      expect(link.href).not.toContain('js.datadome.co');
    });

    it('should not rewrite prefetch links without as="script"', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'prefetch');
      link.setAttribute('as', 'style');
      link.href = 'https://js.datadome.co/tags.js';

      container.appendChild(link);

      expect(link.href).toBe('https://js.datadome.co/tags.js');
    });

    it('should not rewrite non-DataDome prefetch links', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'prefetch');
      link.setAttribute('as', 'script');
      link.href = 'https://example.com/other-script.js';

      container.appendChild(link);

      expect(link.href).toBe('https://example.com/other-script.js');
    });

    it('should work with insertBefore for prefetch links', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const reference = document.createElement('div');
      container.appendChild(reference);

      const link = document.createElement('link');
      link.setAttribute('rel', 'prefetch');
      link.setAttribute('as', 'script');
      link.href = 'https://js.datadome.co/tags.js';

      container.insertBefore(link, reference);

      expect(link.href).toContain('/integrations/datadome/tags.js');
    });
  });

  describe('integration scenarios', () => {
    it('should handle multiple script insertions', () => {
      installDataDomeGuard();

      const container = document.createElement('div');

      const script1 = document.createElement('script');
      script1.src = 'https://js.datadome.co/tags.js';

      const script2 = document.createElement('script');
      script2.src = 'https://example.com/other.js';

      container.appendChild(script1);
      container.appendChild(script2);

      expect(script1.src).toContain('/integrations/datadome/tags.js');
      expect(script2.src).toBe('https://example.com/other.js');
    });

    it('should handle both script and preload link together', () => {
      installDataDomeGuard();

      const container = document.createElement('div');

      // Add preload link first (typical framework behavior)
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.href = 'https://js.datadome.co/tags.js';

      // Add script tag
      const script = document.createElement('script');
      script.src = 'https://js.datadome.co/tags.js';

      container.appendChild(link);
      container.appendChild(script);

      expect(link.href).toContain('/integrations/datadome/tags.js');
      expect(script.src).toContain('/integrations/datadome/tags.js');
      expect(link.href).toBe(script.src); // Should be the same URL
    });

    it('should work with vanilla JavaScript script insertion pattern', () => {
      installDataDomeGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.type = 'text/javascript';
      script.async = true;
      script.src = 'https://js.datadome.co/tags.js';

      container.appendChild(script);

      expect(script.src).toContain('/integrations/datadome/tags.js');
      expect(script.type).toBe('text/javascript');
      expect(script.async).toBe(true);
    });
  });
});
