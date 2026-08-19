import { describe, expect, it, vi } from 'vitest';

import { createApsIntegrationRegistration } from '../../../src/integrations/aps/module';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  PreparedIntegration,
} from '../../../src/kernel/integration_registry';
import type { RenderAttempt } from '../../../src/services/render';
import type { PucApsMountInput } from '../../../src/services/puc_bridge';

const RELEASE_ID = 'a'.repeat(64);

describe('APS provider', () => {
  it('prepares inertly, registers exact owned state during activation, and removes it on rollback', () => {
    const preparationRelease: Array<() => void> = [];
    const activationRelease: Array<() => void> = [];
    let registeredRenderer:
      ((attempt: RenderAttempt, container: HTMLElement) => boolean) | undefined;
    let registeredValidation: Readonly<Record<string, unknown>> | undefined;
    const releaseRenderer = vi.fn(() => {
      registeredRenderer = undefined;
    });
    const releaseValidation = vi.fn(() => {
      registeredValidation = undefined;
    });
    const render = Object.freeze({
      publisherOrigin: window.location.origin,
      rendererNonces: Object.freeze({}),
      registerRenderer: vi.fn(
        (_type: 'aps', renderer: (attempt: RenderAttempt, container: HTMLElement) => boolean) => {
          registeredRenderer = renderer;
          return releaseRenderer;
        }
      ),
    });
    const messages = Object.freeze({
      messaging: Object.freeze({}),
      registerApsValidation: vi.fn((validation: Readonly<Record<string, unknown>>) => {
        registeredValidation = validation;
        return releaseValidation;
      }),
    });
    const registration = createApsIntegrationRegistration(RELEASE_ID);
    const prepared = registration.prepare(
      Object.freeze({
        config: Object.freeze({}),
        interfaces: Object.freeze({ 'render.v1': render, 'messages.v1': messages }),
        signal: new AbortController().signal,
        onDispose: (callback: () => void) => preparationRelease.push(callback),
      } satisfies IntegrationPrepareContext)
    ) as PreparedIntegration;
    const aps = prepared.interfaces?.['aps.v1'] as {
      render: (attempt: RenderAttempt, container: HTMLElement) => boolean;
      renderPuc: (input: PucApsMountInput) => boolean;
    };

    expect(Object.isFrozen(aps)).toBe(true);
    expect(render.registerRenderer).not.toHaveBeenCalled();
    expect(messages.registerApsValidation).not.toHaveBeenCalled();
    expect(aps.render({} as RenderAttempt, document.createElement('div'))).toBe(false);
    expect(aps.renderPuc({} as PucApsMountInput)).toBe(false);

    prepared.activate(
      Object.freeze({
        signal: new AbortController().signal,
        onDispose: (callback: () => void) => activationRelease.push(callback),
        afterCommit: vi.fn(),
      } satisfies IntegrationActivationContext)
    );
    expect(render.registerRenderer).toHaveBeenCalledOnce();
    expect(messages.registerApsValidation).toHaveBeenCalledOnce();
    expect(registeredRenderer).toBe(aps.render);
    expect(registeredValidation).toMatchObject({
      expectedPublisherOrigin: window.location.origin,
      expectedRendererUrl: new URL('/integrations/aps/renderer/v1', window.location.origin).href,
    });

    activationRelease.reverse().forEach((callback) => callback());
    expect(releaseRenderer).toHaveBeenCalledOnce();
    expect(releaseValidation).toHaveBeenCalledOnce();
    expect(registeredRenderer).toBeUndefined();
    expect(registeredValidation).toBeUndefined();
    expect(aps.render({} as RenderAttempt, document.createElement('div'))).toBe(false);
    expect(aps.renderPuc({} as PucApsMountInput)).toBe(false);
    preparationRelease.reverse().forEach((callback) => callback());
  });

  it('pre-registers rollback before either activation mutation can throw', () => {
    const activationRelease: Array<() => void> = [];
    const releaseValidation = vi.fn();
    const render = Object.freeze({
      publisherOrigin: window.location.origin,
      rendererNonces: Object.freeze({}),
      registerRenderer: vi.fn(() => {
        throw new Error('renderer collision');
      }),
    });
    const messages = Object.freeze({
      messaging: Object.freeze({}),
      registerApsValidation: vi.fn(() => releaseValidation),
    });
    const prepared = createApsIntegrationRegistration(RELEASE_ID).prepare(
      Object.freeze({
        config: Object.freeze({}),
        interfaces: Object.freeze({ 'render.v1': render, 'messages.v1': messages }),
        signal: new AbortController().signal,
        onDispose: vi.fn(),
      } satisfies IntegrationPrepareContext)
    ) as PreparedIntegration;

    expect(() =>
      prepared.activate(
        Object.freeze({
          signal: new AbortController().signal,
          onDispose: (callback: () => void) => activationRelease.push(callback),
          afterCommit: vi.fn(),
        } satisfies IntegrationActivationContext)
      )
    ).toThrow('renderer collision');
    activationRelease.reverse().forEach((callback) => callback());
    expect(releaseValidation).toHaveBeenCalledOnce();
  });
});
