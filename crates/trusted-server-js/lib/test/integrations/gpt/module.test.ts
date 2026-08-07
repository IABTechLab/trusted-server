import { afterEach, describe, expect, it, vi } from 'vitest';

import { createGptIntegrationRegistration } from '../../../src/integrations/gpt/module';
import { isGuardInstalled, resetGuardState } from '../../../src/integrations/gpt/script_guard';
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
    integrations: ids.map((id) => ({ id, required: true })),
  };
}

function registration(
  id: string,
  prepare: IntegrationRegistration['prepare']
): IntegrationRegistration {
  return Object.freeze({ id, release: RELEASE_ID, prepare });
}

function callbacks(order: string[]): IntegrationInstallCallbacks {
  return {
    activateCore: () => order.push('core'),
    publish: () => order.push('publish'),
    drainPreload: () => order.push('drain'),
  };
}

describe('transactional GPT integration module', () => {
  afterEach(() => resetGuardState());

  it('prepares inertly, activates the reversible guard, and starts only after commit', async () => {
    const config = Object.freeze({ scriptUrl: '/integrations/gpt/script' });
    const order: string[] = [];
    const start = vi.fn((received: unknown) => {
      order.push('start');
      expect(received).toBe(config);
    });
    let finishPreparation: (() => void) | undefined;
    const preparationGate = new Promise<void>((resolve) => {
      finishPreparation = resolve;
    });
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'gate']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt', 'gate']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({ gpt: Object.freeze({ start }) }),
      }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));
    registry.register(
      registration('gate', async () => {
        order.push('gate:prepare');
        await preparationGate;
        return Object.freeze({ activate: () => order.push('gate:activate') });
      })
    );
    const originalDocumentWrite = document.write;

    const installing = registry.install(callbacks(order));
    await vi.waitFor(() => expect(order).toEqual(['gate:prepare']));

    expect(isGuardInstalled()).toBe(false);
    expect(document.write).toBe(originalDocumentWrite);
    expect(start).not.toHaveBeenCalled();

    finishPreparation?.();
    const result = await installing;

    expect(result).toMatchObject({ state: 'kernel' });
    expect(isGuardInstalled()).toBe(true);
    expect(document.write).not.toBe(originalDocumentWrite);
    expect(order).toEqual(['gate:prepare', 'core', 'gate:activate', 'publish', 'start', 'drain']);
    expect(start).toHaveBeenCalledExactlyOnceWith(config);

    if (result.state === 'kernel') {
      result.dispose();
      result.dispose();
    }
    expect(isGuardInstalled()).toBe(false);
    expect(document.write).toBe(originalDocumentWrite);
  });

  it('unwinds the GPT guard before fallback when a later activation fails', async () => {
    const start = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'broken']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt', 'broken']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: Object.freeze({}),
        interfaces: Object.freeze({ gpt: Object.freeze({ start }) }),
      }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));
    registry.register(
      registration('broken', () => ({
        activate: () => {
          expect(isGuardInstalled()).toBe(true);
          throw new Error('fictional activation failure');
        },
      }))
    );

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });

    expect(isGuardInstalled()).toBe(false);
    expect(start).not.toHaveBeenCalled();
  });

  it('fails preparation without effects when the composition omits the GPT boundary', async () => {
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({ config: Object.freeze({}), interfaces: Object.freeze({}) }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });

    expect(isGuardInstalled()).toBe(false);
  });

  it.each([
    [
      'accessor',
      Object.freeze(
        Object.defineProperty({}, 'scriptUrl', {
          enumerable: true,
          get: () => '/publisher-controlled',
        })
      ),
    ],
    ['mutable nested data', Object.freeze({ nested: {} })],
    ['non-plain data', Object.freeze({ value: Object.freeze(new Date(0)) })],
  ])('rejects %s configuration during inert preparation', async (_caseName, config) => {
    const start = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({ gpt: Object.freeze({ start }) }),
      }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(start).not.toHaveBeenCalled();
    expect(isGuardInstalled()).toBe(false);
  });

  it('isolates post-commit startup failure and disposes only the GPT module', async () => {
    const start = vi.fn(() => {
      throw new Error('fictional GPT startup failure');
    });
    const runtimeFailures: unknown[] = [];
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt']),
      startedAtMs: 0,
      now: () => 0,
      onRuntimeFailure: (failure) => runtimeFailures.push(failure),
      getBindings: () => ({
        config: Object.freeze({}),
        interfaces: Object.freeze({ gpt: Object.freeze({ start }) }),
      }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'kernel',
      runtimeFailures: [{ id: 'gpt', phase: 'after_commit' }],
    });

    expect(start).toHaveBeenCalledTimes(1);
    expect(runtimeFailures).toEqual([{ id: 'gpt', phase: 'after_commit' }]);
    expect(isGuardInstalled()).toBe(false);
  });
});
