import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

describe('ad_trace integration gate', () => {
  beforeEach(() => {
    vi.resetModules();
    document.getElementById('ts-ad-trace-overlay')?.remove();
    delete window.__tsjs_adTraceActive;
    delete window.tsjs;
  });

  afterEach(() => {
    document.getElementById('ts-ad-trace-overlay')?.remove();
    delete window.__tsjs_adTraceActive;
    delete window.tsjs;
  });

  it('leaves API and private recorders absent without the server bootstrap', async () => {
    const { installAdTrace } = await import('../../../src/integrations/ad_trace/index');
    expect(installAdTrace()).toBe(false);
    expect(window.tsjs?.adTrace).toBeUndefined();
    expect(window.tsjs?.recordAdTrace).toBeUndefined();
  });

  it('installs one immutable API and consumes the exact bootstrap', async () => {
    window.__tsjs_adTraceActive = true;
    const { installAdTrace } = await import('../../../src/integrations/ad_trace/index');
    expect(installAdTrace()).toBe(true);
    expect(window.__tsjs_adTraceActive).toBeUndefined();
    expect(Object.isFrozen(window.tsjs?.adTrace)).toBe(true);
    expect(typeof window.tsjs?.recordAdTrace).toBe('function');
    expect(document.querySelectorAll('#ts-ad-trace-overlay')).toHaveLength(1);
    expect(installAdTrace()).toBe(true);
    expect(document.querySelectorAll('#ts-ad-trace-overlay')).toHaveLength(1);
  });

  it('does not accept the legacy tester cookie without bootstrap', async () => {
    document.cookie = 'ts-tester=true; Path=/';
    const { installAdTrace } = await import('../../../src/integrations/ad_trace/index');
    expect(installAdTrace()).toBe(false);
  });
});
