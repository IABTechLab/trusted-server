import { describe, expect, it, vi } from 'vitest';

import {
  createDidomiIntegrationRegistration,
  createDidomiRuntime,
} from '../../../src/integrations/didomi/module';
import { createIntegrationRegistry } from '../../../src/kernel/integration_registry';

const RELEASE_ID = 'a'.repeat(64);
const CRITICAL_SRC = `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`;

describe('transactional Didomi integration module', () => {
  it('sets an absolute SDK path without clobbering publisher config and compare-restores it', () => {
    const config = { custom: 'publisher', sdkPath: 'https://publisher.example/sdk/' };
    const target = {
      didomiConfig: config,
      location: { origin: 'https://news.example' },
    };
    const started = vi.fn();
    const runtime = createDidomiRuntime({ started, target });
    const boot = Object.freeze({ proxyPath: '/integrations/didomi/consent/' });

    const release = runtime.activate(boot);

    expect(config).toEqual({
      custom: 'publisher',
      sdkPath: 'https://news.example/integrations/didomi/consent/',
    });
    runtime.start(boot);
    expect(started).toHaveBeenCalledOnce();
    release();
    release();
    expect(config).toEqual({ custom: 'publisher', sdkPath: 'https://publisher.example/sdk/' });
  });

  it('does not overwrite a publisher replacement during disposal', () => {
    const config = { sdkPath: 'https://publisher.example/original/' };
    const runtime = createDidomiRuntime({
      started: vi.fn(),
      target: { didomiConfig: config, location: { origin: 'https://news.example' } },
    });
    const release = runtime.activate(Object.freeze({ proxyPath: '/integrations/didomi/consent/' }));
    config.sdkPath = 'https://publisher.example/replacement/';

    release();

    expect(config.sdkPath).toBe('https://publisher.example/replacement/');
  });

  it.each([
    ['mutable', { proxyPath: '/integrations/didomi/consent/' }],
    ['relative', Object.freeze({ proxyPath: 'integrations/didomi/consent/' })],
    ['protocol relative', Object.freeze({ proxyPath: '//attacker.example/consent/' })],
    ['backslash authority', Object.freeze({ proxyPath: '/\\attacker.example/consent/' })],
    ['extra', Object.freeze({ proxyPath: '/integrations/didomi/consent/', legacy: true })],
  ])('rejects %s boot config before activation', async (_name, config) => {
    const activate = vi.fn(() => vi.fn());
    const registry = createIntegrationRegistry({
      manifest: {
        version: 1,
        releaseId: RELEASE_ID,
        criticalSrc: CRITICAL_SRC,
        integrations: [{ id: 'didomi', phase: 'critical' }],
      },
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['didomi']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({
          didomi: Object.freeze({ activate, start: vi.fn() }),
        }),
      }),
    });
    registry.register(createDidomiIntegrationRegistration(RELEASE_ID));

    await expect(
      registry.install({ activateCore: vi.fn(), publish: vi.fn(), drainPreload: vi.fn() })
    ).resolves.toMatchObject({ state: 'fallback', reason: 'bundle_partial' });
    expect(activate).not.toHaveBeenCalled();
  });
});
