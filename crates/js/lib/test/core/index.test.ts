import { describe, it, expect, beforeEach, vi } from 'vitest';
import type { TsjsApi } from '../../src/core/types';

describe('core/index', () => {
  beforeEach(async () => {
    await vi.resetModules();
    delete (window as typeof window & { tsjs?: TsjsApi }).tsjs;
  });

  it('initializes tsjs API with expected surface', async () => {
    await import('../../src/core/index');
    const api = (window as typeof window & { tsjs?: TsjsApi }).tsjs;
    expect(api).toBeDefined();
    expect(typeof api!.version).toBe('string');
    expect(typeof api!.setConfig).toBe('function');
    expect(typeof api!.getConfig).toBe('function');
    expect(api!.log).toBeDefined();
  });

  it('setConfig updates config', async () => {
    await import('../../src/core/index');
    const api = (window as typeof window & { tsjs?: TsjsApi }).tsjs!;

    api.setConfig({ mode: 'auction', debug: true });
    const cfg = api.getConfig();
    expect(cfg.mode).toBe('auction');
    expect(cfg.debug).toBe(true);
  });
});
