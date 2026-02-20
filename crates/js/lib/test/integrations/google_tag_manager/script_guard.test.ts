import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import {
  installGtmGuard,
  isGuardInstalled,
  resetGuardState,
  isGtmUrl,
  extractGtmPath,
  rewriteGtmUrl,
} from '../../../src/integrations/google_tag_manager/script_guard';

describe('GTM Script Interception Guard', () => {
  let originalAppendChild: typeof Element.prototype.appendChild;
  let originalInsertBefore: typeof Element.prototype.insertBefore;

  beforeEach(() => {
    originalAppendChild = Element.prototype.appendChild;
    originalInsertBefore = Element.prototype.insertBefore;
    resetGuardState();
  });

  afterEach(() => {
    Element.prototype.appendChild = originalAppendChild;
    Element.prototype.insertBefore = originalInsertBefore;
    resetGuardState();
  });

  describe('isGtmUrl', () => {
    it('should detect www.googletagmanager.com URLs', () => {
      expect(isGtmUrl('https://www.googletagmanager.com/gtm.js?id=GTM-XXXX')).toBe(true);
      expect(isGtmUrl('https://www.googletagmanager.com/gtag/js?id=G-XXXX')).toBe(true);
      expect(isGtmUrl('//www.googletagmanager.com/gtm.js?id=GTM-XXXX')).toBe(true);
      expect(isGtmUrl('http://www.googletagmanager.com/gtm.js?id=GTM-XXXX')).toBe(true);
    });

    it('should detect www.google-analytics.com URLs', () => {
      expect(isGtmUrl('https://www.google-analytics.com/collect')).toBe(true);
      expect(isGtmUrl('https://www.google-analytics.com/g/collect')).toBe(true);
      expect(isGtmUrl('//www.google-analytics.com/collect')).toBe(true);
    });

    it('should be case-insensitive', () => {
      expect(isGtmUrl('https://WWW.GOOGLETAGMANAGER.COM/gtm.js')).toBe(true);
      expect(isGtmUrl('https://WWW.GOOGLE-ANALYTICS.COM/collect')).toBe(true);
    });

    it('should not match without www prefix', () => {
      expect(isGtmUrl('https://googletagmanager.com/gtm.js')).toBe(false);
      expect(isGtmUrl('https://google-analytics.com/collect')).toBe(false);
    });

    it('should not match non-Google URLs', () => {
      expect(isGtmUrl('https://example.com/gtm.js')).toBe(false);
      expect(isGtmUrl('https://cdn.example.com/www.googletagmanager.com.js')).toBe(false);
    });

    it('should handle empty and null values', () => {
      expect(isGtmUrl('')).toBe(false);
      expect(isGtmUrl(null as unknown as string)).toBe(false);
      expect(isGtmUrl(undefined as unknown as string)).toBe(false);
    });
  });

  describe('extractGtmPath', () => {
    it('should extract path from GTM URLs', () => {
      expect(extractGtmPath('https://www.googletagmanager.com/gtm.js')).toBe('/gtm.js');
      expect(extractGtmPath('https://www.googletagmanager.com/gtag/js')).toBe('/gtag/js');
    });

    it('should extract path from GA URLs', () => {
      expect(extractGtmPath('https://www.google-analytics.com/collect')).toBe('/collect');
      expect(extractGtmPath('https://www.google-analytics.com/g/collect')).toBe('/g/collect');
    });

    it('should extract path from protocol-relative URLs', () => {
      expect(extractGtmPath('//www.googletagmanager.com/gtm.js')).toBe('/gtm.js');
      expect(extractGtmPath('//www.google-analytics.com/g/collect')).toBe('/g/collect');
    });

    it('should preserve query strings', () => {
      expect(extractGtmPath('https://www.googletagmanager.com/gtm.js?id=GTM-XXXX')).toBe(
        '/gtm.js?id=GTM-XXXX'
      );
      expect(
        extractGtmPath('https://www.google-analytics.com/g/collect?v=2&tid=G-TEST')
      ).toBe('/g/collect?v=2&tid=G-TEST');
    });

    it('should handle bare domain', () => {
      expect(extractGtmPath('https://www.googletagmanager.com')).toBe('/');
      expect(extractGtmPath('https://www.googletagmanager.com/')).toBe('/');
    });
  });

  describe('rewriteGtmUrl', () => {
    it('should rewrite GTM script URL to first-party', () => {
      const rewritten = rewriteGtmUrl('https://www.googletagmanager.com/gtm.js?id=GTM-XXXX');
      expect(rewritten).toContain('/integrations/google_tag_manager/gtm.js?id=GTM-XXXX');
      expect(rewritten).toContain(window.location.host);
    });

    it('should rewrite GA collect URL to first-party', () => {
      const rewritten = rewriteGtmUrl('https://www.google-analytics.com/g/collect?v=2');
      expect(rewritten).toContain('/integrations/google_tag_manager/g/collect?v=2');
    });

    it('should preserve query strings', () => {
      const rewritten = rewriteGtmUrl(
        'https://www.googletagmanager.com/gtm.js?id=GTM-XXXX&l=dataLayer'
      );
      expect(rewritten).toContain('/integrations/google_tag_manager/gtm.js?id=GTM-XXXX&l=dataLayer');
    });
  });

  describe('installGtmGuard', () => {
    it('should install the guard successfully', () => {
      expect(isGuardInstalled()).toBe(false);
      installGtmGuard();
      expect(isGuardInstalled()).toBe(true);
    });

    it('should not install twice', () => {
      installGtmGuard();
      const firstInstall = Element.prototype.appendChild;
      installGtmGuard();
      const secondInstall = Element.prototype.appendChild;
      expect(firstInstall).toBe(secondInstall);
    });

    it('should patch Element.prototype.appendChild', () => {
      installGtmGuard();
      expect(Element.prototype.appendChild).not.toBe(originalAppendChild);
    });

    it('should patch Element.prototype.insertBefore', () => {
      installGtmGuard();
      expect(Element.prototype.insertBefore).not.toBe(originalInsertBefore);
    });
  });

  describe('appendChild interception', () => {
    it('should rewrite GTM script URL', () => {
      installGtmGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://www.googletagmanager.com/gtm.js?id=GTM-XXXX';

      container.appendChild(script);

      expect(script.src).toContain('/integrations/google_tag_manager/gtm.js?id=GTM-XXXX');
      expect(script.src).not.toContain('googletagmanager.com');
    });

    it('should rewrite Google Analytics script URL', () => {
      installGtmGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://www.google-analytics.com/g/collect';

      container.appendChild(script);

      expect(script.src).toContain('/integrations/google_tag_manager/g/collect');
      expect(script.src).not.toContain('google-analytics.com');
    });

    it('should not rewrite non-GTM scripts', () => {
      installGtmGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://example.com/some-script.js';

      container.appendChild(script);

      expect(script.src).toBe('https://example.com/some-script.js');
    });

    it('should not affect non-script elements', () => {
      installGtmGuard();

      const container = document.createElement('div');
      const img = document.createElement('img');
      img.src = 'https://www.googletagmanager.com/image.png';

      container.appendChild(img);

      expect(img.src).toBe('https://www.googletagmanager.com/image.png');
    });

    it('should preserve other script attributes', () => {
      installGtmGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.setAttribute('async', '');
      script.setAttribute('data-nscript', 'afterInteractive');
      script.src = 'https://www.googletagmanager.com/gtm.js?id=GTM-XXXX';

      container.appendChild(script);

      expect(script.getAttribute('async')).toBe('');
      expect(script.getAttribute('data-nscript')).toBe('afterInteractive');
    });
  });

  describe('insertBefore interception', () => {
    it('should rewrite GTM script URL', () => {
      installGtmGuard();

      const container = document.createElement('div');
      const reference = document.createElement('div');
      container.appendChild(reference);

      const script = document.createElement('script');
      script.src = 'https://www.googletagmanager.com/gtm.js?id=GTM-XXXX';

      container.insertBefore(script, reference);

      expect(script.src).toContain('/integrations/google_tag_manager/gtm.js?id=GTM-XXXX');
      expect(script.src).not.toContain('googletagmanager.com');
    });

    it('should work with null reference node', () => {
      installGtmGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://www.googletagmanager.com/gtm.js?id=GTM-XXXX';

      container.insertBefore(script, null);

      expect(script.src).toContain('/integrations/google_tag_manager/gtm.js?id=GTM-XXXX');
    });
  });

  describe('link preload interception', () => {
    it('should rewrite GTM preload link', () => {
      installGtmGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.href = 'https://www.googletagmanager.com/gtm.js?id=GTM-XXXX';

      container.appendChild(link);

      expect(link.href).toContain('/integrations/google_tag_manager/gtm.js?id=GTM-XXXX');
      expect(link.href).not.toContain('googletagmanager.com');
    });

    it('should not rewrite preload links without as="script"', () => {
      installGtmGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'style');
      link.href = 'https://www.googletagmanager.com/gtm.js?id=GTM-XXXX';

      container.appendChild(link);

      expect(link.href).toBe('https://www.googletagmanager.com/gtm.js?id=GTM-XXXX');
    });

    it('should not rewrite non-GTM preload links', () => {
      installGtmGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.href = 'https://example.com/other-script.js';

      container.appendChild(link);

      expect(link.href).toBe('https://example.com/other-script.js');
    });
  });

  describe('integration scenarios', () => {
    it('should handle the standard GTM snippet pattern (dynamic script creation)', () => {
      installGtmGuard();

      // Simulates the standard GTM snippet: j.src='https://www.googletagmanager.com/gtm.js?id='+i
      const container = document.createElement('div');
      const script = document.createElement('script');
      script.async = true;
      script.src = 'https://www.googletagmanager.com/gtm.js?id=GTM-XXXX';

      container.appendChild(script);

      expect(script.src).toContain('/integrations/google_tag_manager/gtm.js?id=GTM-XXXX');
      expect(script.async).toBe(true);
    });

    it('should handle multiple script insertions', () => {
      installGtmGuard();

      const container = document.createElement('div');

      const script1 = document.createElement('script');
      script1.src = 'https://www.googletagmanager.com/gtm.js?id=GTM-XXXX';

      const script2 = document.createElement('script');
      script2.src = 'https://example.com/other.js';

      container.appendChild(script1);
      container.appendChild(script2);

      expect(script1.src).toContain('/integrations/google_tag_manager/gtm.js?id=GTM-XXXX');
      expect(script2.src).toBe('https://example.com/other.js');
    });
  });
});
