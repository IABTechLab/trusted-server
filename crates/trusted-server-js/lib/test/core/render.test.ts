import { describe, it, expect, beforeEach, vi } from 'vitest';

describe('render', () => {
  beforeEach(async () => {
    await vi.resetModules();
    document.body.innerHTML = '';
  });

  it('creates a sandboxed iframe with sanitized creative HTML via srcdoc', async () => {
    const { createAdIframe, buildCreativeDocument, sanitizeCreativeHtml } =
      await import('../../src/core/render');
    const div = document.createElement('div');
    div.id = 'slotA';
    document.body.appendChild(div);

    const iframe = createAdIframe(div, { name: 'test', width: 300, height: 250 });
    const sanitization = sanitizeCreativeHtml('<span>ad</span>');

    expect(sanitization.kind).toBe('accepted');
    if (sanitization.kind !== 'accepted') {
      throw new Error('should accept safe creative markup');
    }

    iframe.srcdoc = buildCreativeDocument(sanitization.sanitizedHtml);

    expect(iframe).toBeTruthy();
    expect(iframe.srcdoc).toContain('<span>ad</span>');
    expect(div.querySelector('iframe')).toBe(iframe);
    const sandbox = iframe.getAttribute('sandbox') ?? '';
    expect(sandbox).toContain('allow-forms');
    expect(sandbox).toContain('allow-popups');
    expect(sandbox).toContain('allow-popups-to-escape-sandbox');
    expect(sandbox).toContain('allow-top-navigation-by-user-activation');
    expect(sandbox).toContain('allow-scripts');
    // `allow-scripts` + `allow-same-origin` together defeat the sandbox: creative
    // markup would run with the publisher origin's privileges (cookies, storage,
    // same-origin fetches). Matches APS_RENDERER_SANDBOX and ADM_IFRAME_SANDBOX,
    // which already omit it.
    expect(sandbox).not.toContain('allow-same-origin');
  });

  it('preserves dollar sequences when building the creative document', async () => {
    const { buildCreativeDocument } = await import('../../src/core/render');
    const creativeHtml = "<div>$& $$ $1 $` $'</div>";
    const documentHtml = buildCreativeDocument(creativeHtml);

    expect(documentHtml).toContain(creativeHtml);
  });

  it('stamps the first-party origin ahead of the creative markup', async () => {
    // The srcdoc document has an opaque origin and an about:srcdoc location, so
    // the creative runtime has no trustworthy origin of its own. This page —
    // first-party and non-opaque — stamps the real one before any bidder markup
    // can install a <base> or otherwise influence resolution.
    const { buildCreativeDocument } = await import('../../src/core/render');
    const creativeHtml = '<div>creative</div>';
    const documentHtml = buildCreativeDocument(creativeHtml);

    expect(documentHtml).toContain(`value: '${location.origin}'`);
    expect(documentHtml.indexOf('__tsCreativeOrigin')).toBeLessThan(
      documentHtml.indexOf(creativeHtml)
    );
  });

  it('defines the stamped origin so creative script cannot overwrite it', async () => {
    // Creative markup can carry its own <head> script, whose content executes
    // before the runtime injected at the top of <body>. A plain assignment
    // would let it point click and rebuild resolution at an attacker origin.
    const { buildCreativeDocument } = await import('../../src/core/render');
    const documentHtml = buildCreativeDocument('<div>creative</div>');

    expect(documentHtml).toContain("Object.defineProperty(window, '__tsCreativeOrigin'");
    expect(documentHtml).toContain('writable: false');
    expect(documentHtml).toContain('configurable: false');

    // Execute the stamp exactly as the browser would, then try to overwrite it.
    // Parse rather than regex out the script: tag-matching patterns miss case
    // and attribute variations, and the DOM is what the browser actually uses.
    const parsed = new DOMParser().parseFromString(documentHtml, 'text/html');
    const stamp = parsed.querySelector('head script')?.textContent;
    expect(stamp, 'document should carry the stamping script').toBeTruthy();
    const host: Record<string, unknown> = {};
    new Function('window', stamp as string)(host);
    expect(host.__tsCreativeOrigin).toBe(location.origin);
    try {
      host.__tsCreativeOrigin = 'https://attacker.example';
    } catch {
      // strict-mode assignment throws; either way the value must not change
    }
    expect(host.__tsCreativeOrigin).toBe(location.origin);
  });

  it('accepts safe static markup during sanitization', async () => {
    const { sanitizeCreativeHtml } = await import('../../src/core/render');
    const sanitization = sanitizeCreativeHtml(
      '<div><a href="mailto:test@example.com">Contact</a><img src="https://example.com/ad.png" alt="ad creative"></div>'
    );

    expect(sanitization.kind).toBe('accepted');
    if (sanitization.kind !== 'accepted') {
      throw new Error('should accept safe static creative HTML');
    }

    expect(sanitization.sanitizedHtml).toContain('<img');
    expect(sanitization.sanitizedHtml).toContain('mailto:test@example.com');
    expect(sanitization.removedCount).toBe(0);
  });

  it('accepts safe inline styles during sanitization', async () => {
    const { sanitizeCreativeHtml } = await import('../../src/core/render');
    const sanitization = sanitizeCreativeHtml('<div style="color: red">styled creative</div>');

    expect(sanitization.kind).toBe('accepted');
    if (sanitization.kind !== 'accepted') {
      throw new Error('should accept safe inline styles');
    }

    expect(sanitization.sanitizedHtml).toContain('style=');
    expect(sanitization.removedCount).toBe(0);
  });

  it('accepts server-sanitized creative HTML (content-based checks are server-side)', async () => {
    const { sanitizeCreativeHtml } = await import('../../src/core/render');
    // The server strips dangerous markup before adm reaches the client.
    // The client only validates type and emptiness — content passes through.
    const sanitization = sanitizeCreativeHtml(
      '<div><img src="https://cdn.example.com/ad.png" alt="ad"></div>'
    );

    expect(sanitization.kind).toBe('accepted');
  });

  it('rejects malformed non-string creative HTML', async () => {
    const { sanitizeCreativeHtml } = await import('../../src/core/render');
    const sanitization = sanitizeCreativeHtml({ html: '<div>bad</div>' });

    expect(sanitization).toEqual(
      expect.objectContaining({
        kind: 'rejected',
        rejectionReason: 'invalid-creative-html',
      })
    );
  });

  it('rejects creatives that sanitize to empty markup', async () => {
    const { sanitizeCreativeHtml } = await import('../../src/core/render');
    const sanitization = sanitizeCreativeHtml('   ');

    expect(sanitization).toEqual(
      expect.objectContaining({
        kind: 'rejected',
        rejectionReason: 'empty-after-sanitize',
      })
    );
  });
});
