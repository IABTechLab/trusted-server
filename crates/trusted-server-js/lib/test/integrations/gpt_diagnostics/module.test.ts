import { describe, expect, it, vi } from 'vitest';

import { createGptDiagnosticsIntegrationRegistration } from '../../../src/integrations/gpt_diagnostics/module';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  PreparedIntegration,
} from '../../../src/kernel/integration_registry';

const RELEASE_ID = 'a'.repeat(64);

function capabilities(
  subscribe: (listener: (fact: Readonly<Record<string, unknown>>) => void) => () => void = vi.fn(
    () => vi.fn()
  ),
  runtimeDocument: unknown = document
) {
  return Object.freeze({
    'runtime.v1': Object.freeze({ document: runtimeDocument }),
    'gpt.events.v1': Object.freeze({ subscribe }),
  });
}

function prepare(
  interfaces: Readonly<Record<string, unknown>>,
  preparationRelease: Array<() => void> = []
): PreparedIntegration {
  return createGptDiagnosticsIntegrationRegistration(RELEASE_ID).prepare(
    Object.freeze({
      config: Object.freeze({ active: true }),
      interfaces,
      signal: new AbortController().signal,
      onDispose: (callback: () => void) => preparationRelease.push(callback),
    } satisfies IntegrationPrepareContext)
  ) as PreparedIntegration;
}

describe('critical GPT diagnostics data provider', () => {
  it('accepts a valid foreign-realm Document at the registration boundary', () => {
    const frame = document.createElement('iframe');
    document.body.append(frame);
    const foreignDocument = frame.contentDocument;
    const foreignWindow = frame.contentWindow;
    if (!foreignDocument || !foreignWindow) throw new Error('Expected an iframe document realm');
    const foreignRealm = foreignWindow as Window & typeof globalThis;
    expect(foreignDocument).not.toBeInstanceOf(window.Document);
    expect(foreignDocument).toBeInstanceOf(foreignRealm.Document);
    const releases: Array<() => void> = [];

    expect(() => prepare(capabilities(undefined, foreignDocument), releases)).not.toThrow();

    releases.reverse().forEach((release) => release());
    frame.remove();
  });

  it.each([
    ['plain record', Object.freeze({})],
    [
      'counterfeit realm',
      Object.freeze({ defaultView: Object.freeze({ Document: class CounterfeitDocument {} }) }),
    ],
    [
      'hostile defaultView',
      Object.freeze(
        Object.defineProperty({}, 'defaultView', {
          get: () => {
            throw new Error('hostile defaultView');
          },
        })
      ),
    ],
  ])('rejects a %s runtime Document candidate at the registration boundary', (_name, candidate) => {
    expect(() => prepare(capabilities(undefined, candidate))).toThrow(
      'GPT diagnostics requires runtime.v1'
    );
  });

  it('prepares inertly, captures the GPT stream only while active, and exposes no presentation', () => {
    const preparationRelease: Array<() => void> = [];
    const activationRelease: Array<() => void> = [];
    let publish: ((fact: Readonly<Record<string, unknown>>) => void) | undefined;
    const releaseEvents = vi.fn();
    const subscribe = vi.fn((listener: (fact: Readonly<Record<string, unknown>>) => void) => {
      publish = listener;
      return releaseEvents;
    });
    const prepared = prepare(capabilities(subscribe), preparationRelease);
    const data = prepared.interfaces?.['gpt_diag.v1'] as {
      api: {
        snapshot: () => { slots: readonly Readonly<Record<string, unknown>>[] };
      };
      attachPresentation: (controls: Readonly<Record<string, unknown>>) => () => void;
    };

    expect(Reflect.ownKeys(prepared.interfaces ?? {})).toEqual(['gpt_diag.v1']);
    expect(Reflect.ownKeys(data)).toEqual(['api', 'attachPresentation']);
    expect(Object.isFrozen(data)).toBe(true);
    expect(subscribe).not.toHaveBeenCalled();
    expect(data.api.snapshot().slots).toEqual([]);
    expect(document.querySelector('[id^="trusted-server-gpt-diagnostics"]')).toBeNull();

    prepared.activate(
      Object.freeze({
        signal: new AbortController().signal,
        onDispose: (callback: () => void) => activationRelease.push(callback),
        afterCommit: vi.fn(),
      } satisfies IntegrationActivationContext)
    );
    expect(subscribe).toHaveBeenCalledOnce();
    const fact = Object.freeze({
      kind: 'slotRequested',
      observedAtMs: 1,
      slot: Object.freeze({ token: Object.freeze({}) }),
    });
    publish?.(fact);
    expect(data.api.snapshot().slots[0]).toMatchObject({
      runtimeSlotNumber: 1,
      binding: { status: 'unbound', reason: 'missing_element' },
    });
    expect(document.querySelector('[id^="trusted-server-gpt-diagnostics"]')).toBeNull();

    activationRelease.reverse().forEach((callback) => callback());
    expect(releaseEvents).toHaveBeenCalledOnce();
    preparationRelease.reverse().forEach((callback) => callback());
  });

  it('consumes only runtime.v1 and gpt.events.v1 without inspecting trace capabilities', () => {
    const traceRead = vi.fn(() => {
      throw new Error('trace capability must remain unobserved');
    });
    const interfaces = Object.freeze(
      Object.defineProperty(
        {
          'runtime.v1': Object.freeze({ document }),
          'gpt.events.v1': Object.freeze({ subscribe: vi.fn(() => vi.fn()) }),
        },
        'trace.v1',
        { enumerable: true, get: traceRead }
      )
    );

    expect(() => prepare(interfaces)).not.toThrow();
    expect(traceRead).not.toHaveBeenCalled();
  });

  it('pre-registers rollback before the GPT subscription can throw', () => {
    const activationRelease: Array<() => void> = [];
    const prepared = prepare(
      capabilities(
        vi.fn(() => {
          throw new Error('listener collision');
        })
      )
    );

    expect(() =>
      prepared.activate(
        Object.freeze({
          signal: new AbortController().signal,
          onDispose: (callback: () => void) => activationRelease.push(callback),
          afterCommit: vi.fn(),
        } satisfies IntegrationActivationContext)
      )
    ).toThrow('listener collision');
    activationRelease.reverse().forEach((callback) => callback());
    const data = prepared.interfaces?.['gpt_diag.v1'] as {
      api: { snapshot: () => { slots: readonly unknown[] } };
    };
    expect(data.api.snapshot().slots).toEqual([]);
  });

  it.each([
    ['inactive config', Object.freeze({ active: false }), capabilities()],
    ['mutable config', { active: true }, capabilities()],
    [
      'missing GPT event stream',
      Object.freeze({ active: true }),
      Object.freeze({
        'runtime.v1': Object.freeze({ document }),
      }),
    ],
  ])('rejects %s during inert preparation', (_name, config, interfaces) => {
    const registration = createGptDiagnosticsIntegrationRegistration(RELEASE_ID);
    expect(() =>
      registration.prepare(
        Object.freeze({
          config,
          interfaces,
          signal: new AbortController().signal,
          onDispose: vi.fn(),
        } satisfies IntegrationPrepareContext)
      )
    ).toThrow();
  });
});
