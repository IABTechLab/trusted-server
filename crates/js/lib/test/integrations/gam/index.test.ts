import { describe, it, expect, beforeEach, vi } from 'vitest';

describe('integrations/gam', () => {
  beforeEach(async () => {
    await vi.resetModules();
    // Clean up window globals
    delete (window as any).tsGamConfig;
    delete (window as any).__tsGamInstalled;
    delete (window as any).googletag;
    delete (window as any).pbjs;
  });

  it('exports setGamConfig and getGamConfig', async () => {
    const { setGamConfig, getGamConfig } = await import('../../../src/integrations/gam/index');
    expect(typeof setGamConfig).toBe('function');
    expect(typeof getGamConfig).toBe('function');
  });

  it('setGamConfig updates config', async () => {
    const { setGamConfig, getGamConfig } = await import('../../../src/integrations/gam/index');

    setGamConfig({ enabled: true, bidders: ['mocktioneer'], forceRender: true });
    const cfg = getGamConfig();

    expect(cfg.enabled).toBe(true);
    expect(cfg.bidders).toEqual(['mocktioneer']);
    expect(cfg.forceRender).toBe(true);
  });

  it('getGamConfig returns defaults when not configured', async () => {
    const { getGamConfig } = await import('../../../src/integrations/gam/index');
    const cfg = getGamConfig();

    expect(cfg.enabled).toBe(false);
    expect(cfg.bidders).toEqual([]);
    expect(cfg.forceRender).toBe(false);
  });

  it('exports tsGam API object', async () => {
    const { tsGam } = await import('../../../src/integrations/gam/index');

    expect(tsGam).toBeDefined();
    expect(typeof tsGam.setConfig).toBe('function');
    expect(typeof tsGam.getConfig).toBe('function');
    expect(typeof tsGam.getStats).toBe('function');
  });

  it('getStats returns initial empty stats', async () => {
    const { getGamStats } = await import('../../../src/integrations/gam/index');
    const stats = getGamStats();

    expect(stats.intercepted).toBe(0);
    expect(stats.rendered).toEqual([]);
  });

  it('picks up window.tsGamConfig on init', async () => {
    // Set config before importing
    (window as any).tsGamConfig = {
      enabled: true,
      bidders: ['test-bidder'],
      forceRender: false,
    };

    const { getGamConfig } = await import('../../../src/integrations/gam/index');
    const cfg = getGamConfig();

    expect(cfg.enabled).toBe(true);
    expect(cfg.bidders).toEqual(['test-bidder']);
  });

  it('partial config updates preserve existing values', async () => {
    const { setGamConfig, getGamConfig } = await import('../../../src/integrations/gam/index');

    setGamConfig({ enabled: true, bidders: ['bidder1'], forceRender: false });
    setGamConfig({ forceRender: true }); // Only update forceRender

    const cfg = getGamConfig();
    expect(cfg.enabled).toBe(true);
    expect(cfg.bidders).toEqual(['bidder1']);
    expect(cfg.forceRender).toBe(true);
  });

  describe('extractIframeSrc', () => {
    it('extracts src from simple iframe tag', async () => {
      const { extractIframeSrc } = await import('../../../src/integrations/gam/index');

      const html =
        '<iframe src="/first-party/proxy?tsurl=https://example.com" width="300" height="250"></iframe>';
      expect(extractIframeSrc(html)).toBe('/first-party/proxy?tsurl=https://example.com');
    });

    it('handles trailing newline (mocktioneer style)', async () => {
      const { extractIframeSrc } = await import('../../../src/integrations/gam/index');

      const html =
        '<iframe src="/first-party/proxy?tsurl=https%3A%2F%2Flocal.mocktioneer.com" width="728" height="90" frameborder="0" scrolling="no"></iframe>\n';
      expect(extractIframeSrc(html)).toBe(
        '/first-party/proxy?tsurl=https%3A%2F%2Flocal.mocktioneer.com'
      );
    });

    it('returns null for non-iframe content', async () => {
      const { extractIframeSrc } = await import('../../../src/integrations/gam/index');

      expect(extractIframeSrc('<div>not an iframe</div>')).toBeNull();
      expect(extractIframeSrc('<script>alert(1)</script>')).toBeNull();
    });

    it('returns null for iframe without src', async () => {
      const { extractIframeSrc } = await import('../../../src/integrations/gam/index');

      expect(extractIframeSrc('<iframe name="test" width="300"></iframe>')).toBeNull();
    });

    it('returns null for complex content with iframe', async () => {
      const { extractIframeSrc } = await import('../../../src/integrations/gam/index');

      expect(extractIframeSrc('<div><iframe src="https://example.com"></iframe></div>')).toBeNull();
    });
  });
});
