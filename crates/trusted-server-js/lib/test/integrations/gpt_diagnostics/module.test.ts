import { describe, expect, it, vi } from 'vitest';

import { createGptDiagnosticsIntegrationRegistration } from '../../../src/integrations/gpt_diagnostics/module';
import {
  createIntegrationRegistry,
  type IntegrationInstallCallbacks,
} from '../../../src/kernel/integration_registry';

const RELEASE_ID = 'a'.repeat(64);

function manifest() {
  return {
    version: 1,
    releaseId: RELEASE_ID,
    integrations: [{ id: 'gpt_diagnostics', required: true }],
  };
}

function callbacks(order: string[]): IntegrationInstallCallbacks {
  return {
    activateCore: () => order.push('core'),
    publish: () => order.push('publish'),
    drainPreload: () => order.push('drain'),
  };
}

describe('transactional GPT diagnostics integration module', () => {
  it('prepares inertly, activates before publication, and releases exactly once', async () => {
    const order: string[] = [];
    const release = vi.fn(() => order.push('release'));
    const activate = vi.fn(() => {
      order.push('diagnostics:activate');
      return release;
    });
    const runtime = Object.freeze({ activate, currentApi: vi.fn() });
    const registry = createIntegrationRegistry({
      manifest: manifest(),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt_diagnostics']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: Object.freeze({ active: true }),
        interfaces: Object.freeze({ gpt_diagnostics: runtime }),
      }),
    });
    registry.register(createGptDiagnosticsIntegrationRegistration(RELEASE_ID));

    const result = await registry.install(callbacks(order));

    expect(result).toMatchObject({ state: 'kernel' });
    expect(order).toEqual(['core', 'diagnostics:activate', 'publish', 'drain']);
    expect(activate).toHaveBeenCalledOnce();
    if (result.state === 'kernel') {
      result.dispose();
      result.dispose();
    }
    expect(release).toHaveBeenCalledOnce();
  });

  it.each([
    ['inactive', Object.freeze({ active: false })],
    ['extra field', Object.freeze({ active: true, legacy: true })],
    ['mutable', { active: true }],
    ['missing', Object.freeze({})],
  ])('rejects %s configuration without activating', async (_name, config) => {
    const activate = vi.fn(() => vi.fn());
    const registry = createIntegrationRegistry({
      manifest: manifest(),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt_diagnostics']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({
          gpt_diagnostics: Object.freeze({ activate, currentApi: vi.fn() }),
        }),
      }),
    });
    registry.register(createGptDiagnosticsIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(activate).not.toHaveBeenCalled();
  });

  it('rejects a forged composition runtime during inert preparation', async () => {
    const registry = createIntegrationRegistry({
      manifest: manifest(),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt_diagnostics']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: Object.freeze({ active: true }),
        interfaces: Object.freeze({
          gpt_diagnostics: Object.freeze({ activate: vi.fn(), currentApi: vi.fn(), extra: true }),
        }),
      }),
    });
    registry.register(createGptDiagnosticsIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
  });
});
