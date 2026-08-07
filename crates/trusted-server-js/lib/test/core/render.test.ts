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

  it('prepares and appends one exact ADM iframe with srcdoc already assigned', async () => {
    const { ADM_IFRAME_SANDBOX, prepareAdmIframe } = await import('../../src/core/render');
    const container = document.createElement('div');
    document.body.appendChild(container);
    const loaded = vi.fn();
    const failed = vi.fn();
    const observer = new MutationObserver(() => undefined);
    observer.observe(container, { childList: true });
    const handle = prepareAdmIframe({
      adm: '<div>fictional ADM creative</div>',
      container,
      height: 250,
      onError: failed,
      onLoad: loaded,
      width: 300,
    });

    expect(handle).toBeDefined();
    if (!handle) throw new Error('should prepare an ADM iframe');
    expect(handle.frame.parentNode).toBeNull();
    expect(handle.frame.srcdoc).toContain('fictional ADM creative');
    expect(handle.frame.hasAttribute('src')).toBe(false);
    expect(handle.frame.getAttribute('sandbox')).toBe(ADM_IFRAME_SANDBOX);
    expect(handle.frame.referrerPolicy).toBe('no-referrer');
    expect(handle.frame.width).toBe('300');
    expect(handle.frame.height).toBe('250');
    expect(handle.frame.style.width).toBe('300px');
    expect(handle.frame.style.height).toBe('250px');
    expect(handle.append()).toBe(true);
    expect(handle.append()).toBe(false);
    const mutations = observer.takeRecords();
    expect(mutations).toHaveLength(1);
    const inserted = mutations[0]?.addedNodes.item(0) as HTMLIFrameElement | null;
    expect(inserted).toBe(handle.frame);
    expect(inserted?.srcdoc).toBe(handle.frame.srcdoc);
    expect(inserted?.srcdoc.length).toBeGreaterThan(0);
    expect(handle.activate()).toBe(true);
    handle.frame.dispatchEvent(new Event('load'));
    handle.frame.dispatchEvent(new Event('load'));
    expect(loaded).toHaveBeenCalledOnce();
    expect(failed).not.toHaveBeenCalled();
    expect(handle.current()).toBe(true);
    handle.dispose();
    expect(handle.frame.isConnected).toBe(false);
    observer.disconnect();
  });

  it('ignores a poisoned detached factory frame and rejects a pre-append load', async () => {
    const { prepareAdmIframe } = await import('../../src/core/render');
    const poisoned = document.createElement('iframe');
    poisoned.title = 'publisher frame';
    const unrelated = document.createElement('div');
    document.body.appendChild(unrelated);
    poisoned.remove = vi.fn(() => unrelated.remove());
    Object.defineProperty(poisoned, 'srcdoc', {
      configurable: true,
      get: () => '<div>lie</div>',
      set: vi.fn(),
    });
    const container = document.createElement('div');
    document.body.appendChild(container);
    const createElement = vi.spyOn(document, 'createElement').mockReturnValueOnce(poisoned);
    const loaded = vi.fn();
    const failed = vi.fn();

    try {
      const handle = prepareAdmIframe({
        adm: '<div>exact creative</div>',
        container,
        height: 250,
        onError: failed,
        onLoad: loaded,
        width: 300,
      });
      expect(handle).toBeDefined();
      if (!handle) throw new Error('should prepare a native ADM iframe');
      expect(createElement).not.toHaveBeenCalled();
      expect(handle.frame).not.toBe(poisoned);
      handle.frame.dispatchEvent(new Event('load'));
      expect(handle.append()).toBe(true);
      expect(handle.activate()).toBe(true);
      expect(loaded).not.toHaveBeenCalled();
      expect(failed).not.toHaveBeenCalled();
      handle.dispose();
      expect(poisoned.remove).not.toHaveBeenCalled();
      expect(poisoned.title).toBe('publisher frame');
      expect(unrelated.isConnected).toBe(true);
    } finally {
      createElement.mockRestore();
    }
  });

  it('commits only predecessors and keeps the accepted frame exactly disposable', async () => {
    const { prepareAdmIframe } = await import('../../src/core/render');
    const container = document.createElement('div');
    const predecessor = document.createElement('div');
    const laterSibling = document.createElement('div');
    container.appendChild(predecessor);
    document.body.appendChild(container);
    const handle = prepareAdmIframe({
      adm: '<div>accepted creative</div>',
      container,
      height: 250,
      onError: vi.fn(),
      onLoad: vi.fn(),
      width: 300,
    });

    expect(handle).toBeDefined();
    if (!handle) throw new Error('should prepare an ADM iframe');
    expect(handle.append()).toBe(true);
    container.appendChild(laterSibling);
    expect(handle.activate()).toBe(true);
    handle.frame.dispatchEvent(new Event('load'));
    expect(handle.commit()).toBe(true);
    expect(predecessor.isConnected).toBe(false);
    expect(laterSibling.isConnected).toBe(true);
    expect(handle.frame.isConnected).toBe(true);

    handle.dispose();
    handle.dispose();
    expect(handle.frame.isConnected).toBe(false);
    expect(laterSibling.isConnected).toBe(true);
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

    expect(documentHtml).toContain(`window.__tsCreativeOrigin = '${location.origin}'`);
    expect(documentHtml.indexOf('__tsCreativeOrigin')).toBeLessThan(
      documentHtml.indexOf(creativeHtml)
    );
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
