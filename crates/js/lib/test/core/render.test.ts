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
    expect(sandbox).not.toContain('allow-same-origin');
    expect(sandbox).not.toContain('allow-scripts');
  });

  it('preserves dollar sequences when building the creative document', async () => {
    const { buildCreativeDocument } = await import('../../src/core/render');
    const creativeHtml = "<div>$& $$ $1 $` $'</div>";
    const documentHtml = buildCreativeDocument(creativeHtml);

    expect(documentHtml).toContain(creativeHtml);
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

  it('rejects creatives when executable content is stripped', async () => {
    const { sanitizeCreativeHtml } = await import('../../src/core/render');
    const sanitization = sanitizeCreativeHtml('<div onclick="alert(1)">danger</div>');

    expect(sanitization).toEqual(
      expect.objectContaining({
        kind: 'rejected',
        rejectionReason: 'removed-dangerous-content',
      })
    );
  });

  it('rejects creatives with dangerous URI attributes', async () => {
    const { sanitizeCreativeHtml } = await import('../../src/core/render');
    const sanitization = sanitizeCreativeHtml('<a href="javascript:alert(1)">danger</a>');

    expect(sanitization).toEqual(
      expect.objectContaining({
        kind: 'rejected',
        rejectionReason: 'removed-dangerous-content',
      })
    );
  });

  it('rejects creatives with dangerous data HTML image sources', async () => {
    const { sanitizeCreativeHtml } = await import('../../src/core/render');
    const sanitization = sanitizeCreativeHtml(
      '<img src="data:text/html,<script>alert(1)</script>" alt="danger">'
    );

    expect(sanitization).toEqual(
      expect.objectContaining({
        kind: 'rejected',
        rejectionReason: 'removed-dangerous-content',
      })
    );
  });

  it('rejects creatives with dangerous inline styles that survive sanitization', async () => {
    const { sanitizeCreativeHtml } = await import('../../src/core/render');
    const sanitization = sanitizeCreativeHtml(
      '<div style="background-image:url(javascript:alert(1))">danger</div>'
    );

    expect(sanitization).toEqual(
      expect.objectContaining({
        kind: 'rejected',
        rejectionReason: 'removed-dangerous-content',
      })
    );
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
