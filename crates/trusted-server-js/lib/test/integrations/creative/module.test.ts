import { beforeEach, describe, expect, it, vi } from 'vitest';

const ownedGuards = vi.hoisted(() => ({
  installClick: vi.fn(),
  installIframe: vi.fn(),
  installImage: vi.fn(),
}));

vi.mock('../../../src/integrations/creative/click', () => ({
  installClickGuard: ownedGuards.installClick,
}));
vi.mock('../../../src/integrations/creative/iframe', () => ({
  installDynamicIframeProxy: ownedGuards.installIframe,
}));
vi.mock('../../../src/integrations/creative/image', () => ({
  installDynamicImageProxy: ownedGuards.installImage,
}));

import { createCreativeIntegrationRegistration } from '../../../src/integrations/creative/module';
import {
  createIntegrationRegistry,
  type IntegrationInstallCallbacks,
  type IntegrationRegistration,
} from '../../../src/kernel/integration_registry';

const RELEASE_ID = 'a'.repeat(64);

function manifest(ids: readonly string[]) {
  return {
    version: 1,
    releaseId: RELEASE_ID,
    firstDisplay: null,
    runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`,
    integrations: ids.map((id) => ({ id, phase: 'takeover' as const })),
  };
}

function catalog(ids: readonly string[]) {
  return Object.freeze(
    ids.map((id) =>
      Object.freeze({
        id,
        phase: 'takeover' as const,
        trigger: null,
        consumes: Object.freeze(id === 'creative' ? ['runtime.v1'] : []),
        provides: Object.freeze([]),
      })
    )
  );
}

function callbacks(order: string[]): IntegrationInstallCallbacks {
  return {
    activateCore: () => order.push('core'),
    publish: () => order.push('publish'),
    drainPreload: () => order.push('drain'),
  };
}

function registration(
  id: string,
  prepare: IntegrationRegistration['prepare']
): IntegrationRegistration {
  return Object.freeze({ abi: 1, id, phase: 'takeover', releaseId: RELEASE_ID, prepare });
}

function runtimeCapability() {
  return Object.freeze({ document });
}

function guard(name: string, order: string[]) {
  return Object.freeze({
    dispose: vi.fn(() => order.push(`dispose:${name}`)),
    scan: vi.fn(() => order.push(`scan:${name}`)),
  });
}

describe('transactional creative integration module', () => {
  beforeEach(() => {
    ownedGuards.installClick.mockReset();
    ownedGuards.installIframe.mockReset();
    ownedGuards.installImage.mockReset();
  });

  it('prepares inertly, activates reversible guards, and scans only after commit', async () => {
    const config = Object.freeze({
      version: 1,
      enabled: true,
      clickGuard: true,
      renderGuard: true,
    });
    const order: string[] = [];
    const click = guard('click', order);
    const image = guard('image', order);
    const iframe = guard('iframe', order);
    ownedGuards.installClick.mockImplementation(() => {
      order.push('install:click');
      return click;
    });
    ownedGuards.installImage.mockImplementation(() => {
      order.push('install:image');
      return image;
    });
    ownedGuards.installIframe.mockImplementation(() => {
      order.push('install:iframe');
      return iframe;
    });
    let finishPreparation: (() => void) | undefined;
    const preparationGate = new Promise<void>((resolve) => {
      finishPreparation = resolve;
    });
    const registry = createIntegrationRegistry({
      manifest: manifest(['creative', 'gate']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['creative', 'gate']),
      catalog: catalog(['creative', 'gate']),
      startedAtMs: 0,
      now: () => 0,
      runtimeCapability: runtimeCapability(),
      getBindings: () => ({
        config,
        interfaces: Object.freeze({}),
      }),
    });
    registry.register(createCreativeIntegrationRegistration(RELEASE_ID));
    registry.register(
      registration('gate', async () => {
        order.push('gate:prepare');
        await preparationGate;
        return Object.freeze({ activate: () => order.push('gate:activate') });
      })
    );

    const installing = registry.install(callbacks(order));
    await vi.waitFor(() => expect(order).toEqual(['gate:prepare']));
    expect(ownedGuards.installClick).not.toHaveBeenCalled();

    finishPreparation?.();
    const result = await installing;

    expect(result).toMatchObject({ state: 'kernel' });
    expect(order).toEqual([
      'gate:prepare',
      'core',
      'install:click',
      'install:image',
      'install:iframe',
      'gate:activate',
      'publish',
      'scan:click',
      'scan:image',
      'scan:iframe',
      'drain',
    ]);
    if (result.state === 'kernel') {
      result.dispose();
      result.dispose();
    }
    expect(order.slice(-3)).toEqual(['dispose:iframe', 'dispose:image', 'dispose:click']);
  });

  it('performs no runtime work when enabled with both guards false', async () => {
    const config = Object.freeze({
      version: 1 as const,
      enabled: true,
      clickGuard: false,
      renderGuard: false,
    });
    const registry = createIntegrationRegistry({
      manifest: manifest(['creative']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['creative']),
      catalog: catalog(['creative']),
      startedAtMs: 0,
      now: () => 0,
      runtimeCapability: runtimeCapability(),
      getBindings: () => ({
        config,
        interfaces: Object.freeze({}),
      }),
    });
    registry.register(createCreativeIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({ state: 'kernel' });
    expect(ownedGuards.installClick).not.toHaveBeenCalled();
    expect(ownedGuards.installImage).not.toHaveBeenCalled();
    expect(ownedGuards.installIframe).not.toHaveBeenCalled();
  });

  it('unwinds creative activation before a later module failure', async () => {
    const order: string[] = [];
    const click = guard('click', order);
    ownedGuards.installClick.mockReturnValue(click);
    const registry = createIntegrationRegistry({
      manifest: manifest(['creative', 'broken']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['creative', 'broken']),
      catalog: catalog(['creative', 'broken']),
      startedAtMs: 0,
      now: () => 0,
      runtimeCapability: runtimeCapability(),
      getBindings: () => ({
        config: Object.freeze({
          version: 1,
          enabled: true,
          clickGuard: true,
          renderGuard: false,
        }),
        interfaces: Object.freeze({}),
      }),
    });
    registry.register(createCreativeIntegrationRegistration(RELEASE_ID));
    registry.register(
      registration('broken', () => ({
        activate: () => {
          throw new Error('fictional creative peer failure');
        },
      }))
    );

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(click.dispose).toHaveBeenCalledTimes(1);
    expect(click.scan).not.toHaveBeenCalled();
  });

  it.each([
    ['missing field', Object.freeze({ version: 1, enabled: true, clickGuard: true })],
    [
      'unknown field',
      Object.freeze({
        version: 1,
        enabled: true,
        clickGuard: true,
        renderGuard: false,
        extra: true,
      }),
    ],
    [
      'accessor',
      Object.freeze(
        Object.defineProperty({ version: 1, enabled: true, clickGuard: true }, 'renderGuard', {
          enumerable: true,
          get: () => false,
        })
      ),
    ],
    [
      'non-plain object',
      Object.freeze(
        Object.assign(Object.create({ inherited: true }) as object, {
          version: 1,
          enabled: true,
          clickGuard: true,
          renderGuard: false,
        })
      ),
    ],
    ['mutable object', { version: 1, enabled: true, clickGuard: true, renderGuard: false }],
    [
      'disabled click guard',
      Object.freeze({ version: 1, enabled: false, clickGuard: true, renderGuard: false }),
    ],
    [
      'disabled render guard',
      Object.freeze({ version: 1, enabled: false, clickGuard: false, renderGuard: true }),
    ],
  ])('rejects %s configuration during inert preparation', async (_caseName, config) => {
    const activate = vi.fn();
    const start = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['creative']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['creative']),
      catalog: catalog(['creative']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({ creative: Object.freeze({ activate, start }) }),
      }),
    });
    registry.register(createCreativeIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(activate).not.toHaveBeenCalled();
    expect(start).not.toHaveBeenCalled();
  });

  it('fails preparation without effects when composition omits runtime.v1', async () => {
    const registry = createIntegrationRegistry({
      manifest: manifest(['creative']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['creative']),
      catalog: catalog(['creative']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: Object.freeze({
          version: 1,
          enabled: true,
          clickGuard: true,
          renderGuard: false,
        }),
        interfaces: Object.freeze({}),
      }),
    });
    registry.register(createCreativeIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
  });

  it('contains a post-commit guard scan failure inside the creative module', async () => {
    const runtimeFailures: unknown[] = [];
    const order: string[] = [];
    const click = guard('click', order);
    vi.mocked(click.scan).mockImplementation(() => {
      throw new Error('fictional creative scan failure');
    });
    ownedGuards.installClick.mockReturnValue(click);
    const registry = createIntegrationRegistry({
      manifest: manifest(['creative']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['creative']),
      catalog: catalog(['creative']),
      startedAtMs: 0,
      now: () => 0,
      runtimeCapability: runtimeCapability(),
      onRuntimeFailure: (failure) => runtimeFailures.push(failure),
      getBindings: () => ({
        config: Object.freeze({
          version: 1,
          enabled: true,
          clickGuard: true,
          renderGuard: false,
        }),
        interfaces: Object.freeze({}),
      }),
    });
    registry.register(createCreativeIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'kernel',
      runtimeFailures: [],
    });
    expect(click.scan).toHaveBeenCalledOnce();
    expect(runtimeFailures).toEqual([]);
  });
});
