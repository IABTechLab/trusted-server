import { describe, it, expect, beforeEach, vi } from 'vitest';

describe('render', () => {
  beforeEach(async () => {
    await vi.resetModules();
    document.body.innerHTML = '';
  });

  it('creates a sandboxed iframe with creative HTML via srcdoc', async () => {
    const { createAdIframe, buildCreativeDocument } = await import('../../src/core/render');
    const div = document.createElement('div');
    div.id = 'slotA';
    document.body.appendChild(div);

    const iframe = createAdIframe(div, { name: 'test', width: 300, height: 250 });
    iframe.srcdoc = buildCreativeDocument('<span>ad</span>');

    expect(iframe).toBeTruthy();
    expect(iframe.srcdoc).toContain('<span>ad</span>');
    expect(div.querySelector('iframe')).toBe(iframe);
  });
});
