import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { installNextJsGuard, isGuardInstalled, resetGuardState } from '../../../src/integrations/lockr/nextjs_guard';

describe('Lockr SDK Script Interception Guard', () => {
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

  describe('installNextJsGuard', () => {
    it('should install the guard successfully', () => {
      expect(isGuardInstalled()).toBe(false);

      installNextJsGuard();

      expect(isGuardInstalled()).toBe(true);
    });

    it('should not install twice', () => {
      installNextJsGuard();
      const firstInstall = Element.prototype.appendChild;

      installNextJsGuard();
      const secondInstall = Element.prototype.appendChild;

      // Should be the same reference (no double patching)
      expect(firstInstall).toBe(secondInstall);
    });

    it('should patch Element.prototype.appendChild', () => {
      installNextJsGuard();

      expect(Element.prototype.appendChild).not.toBe(originalAppendChild);
    });

    it('should patch Element.prototype.insertBefore', () => {
      installNextJsGuard();

      expect(Element.prototype.insertBefore).not.toBe(originalInsertBefore);
    });
  });

  describe('appendChild interception', () => {
    it('should rewrite Lockr SDK URL from aim.loc.kr', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.appendChild(script);

      expect(script.src).toContain('/integrations/lockr/sdk');
      expect(script.src).not.toContain('aim.loc.kr');
    });

    it('should rewrite Lockr SDK URL from identity.loc.kr', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://identity.loc.kr/identity-lockr-v2.0.js';

      container.appendChild(script);

      expect(script.src).toContain('/integrations/lockr/sdk');
      expect(script.src).not.toContain('identity.loc.kr');
    });

    it('should use location.host for rewritten URL', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.appendChild(script);

      expect(script.src).toContain(window.location.host);
      expect(script.src).toMatch(/^https?:\/\//);
    });

    it('should not rewrite non-Lockr scripts', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://example.com/some-script.js';

      container.appendChild(script);

      expect(script.src).toBe('https://example.com/some-script.js');
    });

    it('should rewrite Lockr scripts regardless of data-nscript attribute', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      
      script.src = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.appendChild(script);

      expect(script.src).toContain('/integrations/lockr/sdk');
      expect(script.src).not.toContain('aim.loc.kr');
    });

    it('should rewrite Lockr scripts with ANY data-nscript value', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.setAttribute('data-nscript', 'beforeInteractive');
      script.src = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.appendChild(script);

      expect(script.src).toContain('/integrations/lockr/sdk');
      expect(script.src).not.toContain('aim.loc.kr');
    });

    it('should rewrite plain scripts without any framework attributes', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://aim.loc.kr/identity-lockr-v1.0.js';
      // No framework attributes at all

      container.appendChild(script);

      expect(script.src).toContain('/integrations/lockr/sdk');
      expect(script.src).not.toContain('aim.loc.kr');
    });

    it('should not affect non-script elements', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const img = document.createElement('img');
      img.src = 'https://aim.loc.kr/image.png';

      container.appendChild(img);

      expect(img.src).toBe('https://aim.loc.kr/image.png');
    });

    it('should handle scripts with setAttribute instead of property', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.setAttribute('src', 'https://aim.loc.kr/identity-lockr-v1.0.js');

      container.appendChild(script);

      expect(script.getAttribute('src')).toContain('/integrations/lockr/sdk');
    });

    it('should work with vanilla JavaScript script insertion', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.type = 'text/javascript';
      script.async = true;
      script.src = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.appendChild(script);

      expect(script.src).toContain('/integrations/lockr/sdk');
      expect(script.type).toBe('text/javascript');
      expect(script.async).toBe(true);
    });
  });

  describe('insertBefore interception', () => {
    it('should rewrite Lockr SDK URL', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const reference = document.createElement('div');
      container.appendChild(reference);

      const script = document.createElement('script');
      script.src = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.insertBefore(script, reference);

      expect(script.src).toContain('/integrations/lockr/sdk');
      expect(script.src).not.toContain('aim.loc.kr');
    });

    it('should not rewrite non-Lockr scripts', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const reference = document.createElement('div');
      container.appendChild(reference);

      const script = document.createElement('script');
      script.src = 'https://example.com/some-script.js';

      container.insertBefore(script, reference);

      expect(script.src).toBe('https://example.com/some-script.js');
    });

    it('should work with null reference node', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.insertBefore(script, null);

      expect(script.src).toContain('/integrations/lockr/sdk');
    });
  });

  describe('URL detection', () => {
    it('should detect aim.loc.kr URLs', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      
      script.src = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.appendChild(script);

      expect(script.src).toContain('/integrations/lockr/sdk');
    });

    it('should detect identity.loc.kr with identity-lockr URLs', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      
      script.src = 'https://identity.loc.kr/identity-lockr-v2.0.js';

      container.appendChild(script);

      expect(script.src).toContain('/integrations/lockr/sdk');
    });

    it('should handle case-insensitive URLs', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      
      script.src = 'https://AIM.LOC.KR/identity-lockr-v1.0.js';

      container.appendChild(script);

      expect(script.src).toContain('/integrations/lockr/sdk');
    });

    it('should not match identity.loc.kr without identity-lockr', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      
      script.src = 'https://identity.loc.kr/other-script.js';

      container.appendChild(script);

      expect(script.src).toBe('https://identity.loc.kr/other-script.js');
    });

    it('should not match identity.loc.kr with identity-lockr but wrong extension', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      
      script.src = 'https://identity.loc.kr/identity-lockr-v1.0.css';

      container.appendChild(script);

      expect(script.src).toBe('https://identity.loc.kr/identity-lockr-v1.0.css');
    });
  });

  describe('link preload interception', () => {
    it('should rewrite Lockr SDK preload link from aim.loc.kr', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.href = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.appendChild(link);

      expect(link.href).toContain('/integrations/lockr/sdk');
      expect(link.href).not.toContain('aim.loc.kr');
    });

    it('should rewrite Lockr SDK preload link from identity.loc.kr', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.href = 'https://identity.loc.kr/identity-lockr-v2.0.js';

      container.appendChild(link);

      expect(link.href).toContain('/integrations/lockr/sdk');
      expect(link.href).not.toContain('identity.loc.kr');
    });

    it('should use location.host for rewritten preload URL', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.href = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.appendChild(link);

      expect(link.href).toContain(window.location.host);
      expect(link.href).toMatch(/^https?:\/\//);
    });

    it('should not rewrite preload links without as="script"', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'style');
      link.href = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.appendChild(link);

      expect(link.href).toBe('https://aim.loc.kr/identity-lockr-v1.0.js');
    });

    it('should not rewrite links without rel="preload"', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'stylesheet');
      link.setAttribute('as', 'script');
      link.href = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.appendChild(link);

      expect(link.href).toBe('https://aim.loc.kr/identity-lockr-v1.0.js');
    });

    it('should not rewrite non-Lockr preload links', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.href = 'https://example.com/other-script.js';

      container.appendChild(link);

      expect(link.href).toBe('https://example.com/other-script.js');
    });

    it('should work with insertBefore for preload links', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const reference = document.createElement('div');
      container.appendChild(reference);

      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.href = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.insertBefore(link, reference);

      expect(link.href).toContain('/integrations/lockr/sdk');
    });

    it('should handle preload link with setAttribute instead of property', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.setAttribute('href', 'https://aim.loc.kr/identity-lockr-v1.0.js');

      container.appendChild(link);

      expect(link.getAttribute('href')).toContain('/integrations/lockr/sdk');
    });

    it('should preserve other link attributes', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.setAttribute('crossorigin', 'anonymous');
      link.setAttribute('id', 'lockr-preload');
      link.href = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.appendChild(link);

      expect(link.getAttribute('rel')).toBe('preload');
      expect(link.getAttribute('as')).toBe('script');
      expect(link.getAttribute('crossorigin')).toBe('anonymous');
      expect(link.getAttribute('id')).toBe('lockr-preload');
    });
  });

  describe('integration scenarios', () => {
    it('should handle multiple script insertions', () => {
      installNextJsGuard();

      const container = document.createElement('div');

      const script1 = document.createElement('script');
      script1.src = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      const script2 = document.createElement('script');
      script2.src = 'https://example.com/other.js';

      container.appendChild(script1);
      container.appendChild(script2);

      expect(script1.src).toContain('/integrations/lockr/sdk');
      expect(script2.src).toBe('https://example.com/other.js');
    });

    it('should preserve other script attributes', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.setAttribute('async', '');
      script.setAttribute('crossorigin', 'anonymous');
      script.setAttribute('id', 'lockr-sdk');
      script.setAttribute('data-framework', 'nextjs'); // Any custom attribute
      script.src = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.appendChild(script);

      expect(script.getAttribute('async')).toBe('');
      expect(script.getAttribute('crossorigin')).toBe('anonymous');
      expect(script.getAttribute('id')).toBe('lockr-sdk');
      expect(script.getAttribute('data-framework')).toBe('nextjs');
    });

    it('should work with scripts created and inserted immediately', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const script = document.createElement('script');
      script.src = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      // Immediate insertion (common pattern)
      container.appendChild(script);

      expect(script.src).toContain('/integrations/lockr/sdk');
    });

    it('should handle both script and preload link together', () => {
      installNextJsGuard();

      const container = document.createElement('div');

      // Add preload link first (typical framework behavior)
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.href = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      // Add script tag
      const script = document.createElement('script');
      script.src = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      // Immediate insertion (common in Next.js)
      container.appendChild(script);

      expect(script.src).toContain('/integrations/lockr/sdk');
    });

    it('should handle both script and preload link together', () => {
      installNextJsGuard();

      const container = document.createElement('div');

      // Add preload link first (typical Next.js behavior)
      const link = document.createElement('link');
      link.setAttribute('rel', 'preload');
      link.setAttribute('as', 'script');
      link.href = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      // Add script tag
      const script = document.createElement('script');
      
      script.src = 'https://aim.loc.kr/identity-lockr-v1.0.js';

      container.appendChild(link);
      container.appendChild(script);

      expect(link.href).toContain('/integrations/lockr/sdk');
      expect(script.src).toContain('/integrations/lockr/sdk');
      expect(link.href).toBe(script.src); // Should be the same URL
    });

    it('should not affect non-preload links', () => {
      installNextJsGuard();

      const container = document.createElement('div');
      const link = document.createElement('link');
      link.setAttribute('rel', 'stylesheet');
      link.href = 'https://aim.loc.kr/styles.css';

      container.appendChild(link);

      expect(link.href).toBe('https://aim.loc.kr/styles.css');
    });
  });
});
