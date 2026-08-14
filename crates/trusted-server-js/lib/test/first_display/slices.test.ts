import { describe, expect, it, vi } from 'vitest';
import { JSDOM } from 'jsdom';

import {
  INITIAL_SLICE_DEFINITIONS,
  selectInitialSliceDefinitions,
} from '../../src/first_display/composition';
import { DIDOMI_INITIAL_SLICE } from '../../src/first_display/slices/didomi';
import { TESTLIGHT_INITIAL_SLICE } from '../../src/first_display/slices/testlight';
import { installDidomiInitial } from '../../src/first_display/leaf/config_guard';
import {
  registerFirstDisplayComponent,
  type FirstDisplayComponentRegistrationV1,
} from '../../src/first_display/registration';
import { createDidomiRuntime } from '../../src/integrations/didomi/module';

const RELEASE_ID = 'a'.repeat(64);

function componentRegistration(): FirstDisplayComponentRegistrationV1 {
  return Object.freeze({
    abi: 1,
    id: 'gpt_initial',
    releaseId: RELEASE_ID,
    order: 7,
    prepare: () => Object.freeze({ activate: () => undefined }),
  });
}

describe('first-display initial slice definitions', () => {
  it('pins the exact twelve optional slices in build order', () => {
    expect(INITIAL_SLICE_DEFINITIONS.map(({ id }) => id)).toEqual([
      'aps_initial',
      'creative_initial',
      'datadome_initial',
      'didomi_initial',
      'google_tag_manager_initial',
      'gpt_initial',
      'lockr_initial',
      'osano_initial',
      'permutive_initial',
      'sourcepoint_initial',
      'prebid_initial',
      'testlight_initial',
    ]);
  });

  it('prepares inertly and activates only its exact host obligation', () => {
    const events: string[] = [];
    const dispose = vi.fn();
    const host = Object.freeze({
      activate: (id: string, own: (callback: () => void) => void) => {
        own(dispose);
        events.push(id);
      },
    });

    const prepared = INITIAL_SLICE_DEFINITIONS.map((definition) => definition.prepare(host));
    expect(events).toEqual([]);
    for (const slice of prepared)
      slice.activate(
        Object.freeze({ own: () => undefined, afterActivate: () => undefined })
      );
    expect(events).toEqual(INITIAL_SLICE_DEFINITIONS.map(({ id }) => id));
    expect(
      INITIAL_SLICE_DEFINITIONS.every(
        (definition) => Reflect.ownKeys(definition).join(',') === 'id,prepare'
      )
    ).toBe(true);
  });

  it('rejects unknown, duplicate, omitted-base, and misordered selections', () => {
    expect(selectInitialSliceDefinitions(['first_display', 'gpt_initial'])?.map(({ id }) => id)).toEqual([
      'gpt_initial',
    ]);
    expect(selectInitialSliceDefinitions(['gpt_initial'])).toBeUndefined();
    expect(selectInitialSliceDefinitions(['first_display', 'gpt_initial', 'gpt_initial'])).toBeUndefined();
    expect(selectInitialSliceDefinitions(['first_display', 'prebid_initial', 'gpt_initial'])).toBeUndefined();
    expect(
      selectInitialSliceDefinitions(['first_display', 'unknown_initial' as 'gpt_initial'])
    ).toBeUndefined();
  });

  it('registers through only the bootstrap-owned sink on the exact current script', () => {
    const dom = new JSDOM(
      `<!doctype html><script id="trustedserver-js" src="/static/tsjs=tsjs-first-display.min.js?m=0041&v=${'b'.repeat(64)}"></script>`,
      { url: 'https://publisher.example/' }
    );
    const script = dom.window.document.querySelector('script') as HTMLScriptElement;
    Object.defineProperty(dom.window.document, 'currentScript', {
      configurable: true,
      value: script,
    });
    const target = {};
    const sink = vi.fn(function (this: unknown, registration: unknown, source: unknown) {
      expect(this).toBe(target);
      expect(registration).toBe(componentRegistrationValue);
      expect(source).toBe(script);
      return true;
    });
    Object.defineProperty(target, '_registerFirstDisplay', {
      configurable: true,
      enumerable: false,
      value: sink,
      writable: false,
    });
    Object.defineProperty(dom.window, 'tsjs', {
      configurable: true,
      enumerable: true,
      value: target,
      writable: true,
    });
    const componentRegistrationValue = componentRegistration();

    expect(
      registerFirstDisplayComponent(
        dom.window as unknown as Window,
        componentRegistrationValue
      )
    ).toBe(true);
    expect(sink).toHaveBeenCalledTimes(1);
  });

  it('rejects an accessor/inherited sink and a noncanonical or detached current script', () => {
    const make = () => {
      const dom = new JSDOM(
        `<!doctype html><script id="trustedserver-js" src="/static/tsjs=tsjs-first-display.min.js?m=0041&v=${'b'.repeat(64)}"></script>`,
        { url: 'https://publisher.example/' }
      );
      const script = dom.window.document.querySelector('script') as HTMLScriptElement;
      Object.defineProperty(dom.window.document, 'currentScript', {
        configurable: true,
        value: script,
      });
      return { dom, script };
    };

    const accessor = make();
    Object.defineProperty(accessor.dom.window, 'tsjs', {
      configurable: true,
      value: Object.defineProperty({}, '_registerFirstDisplay', {
        configurable: true,
        get: vi.fn(),
      }),
    });
    expect(
      registerFirstDisplayComponent(
        accessor.dom.window as unknown as Window,
        componentRegistration()
      )
    ).toBe(false);

    const inherited = make();
    const prototype = Object.defineProperty({}, '_registerFirstDisplay', {
      value: () => true,
    });
    Object.defineProperty(inherited.dom.window, 'tsjs', {
      configurable: true,
      value: Object.create(prototype),
    });
    expect(
      registerFirstDisplayComponent(
        inherited.dom.window as unknown as Window,
        componentRegistration()
      )
    ).toBe(false);

    const wrongSource = make();
    wrongSource.script.src = `https://publisher.example/static/tsjs=tsjs-first-display.min.js?v=${'b'.repeat(64)}&m=0041`;
    Object.defineProperty(wrongSource.dom.window, 'tsjs', {
      configurable: true,
      value: Object.defineProperty({}, '_registerFirstDisplay', {
        value: () => true,
      }),
    });
    expect(
      registerFirstDisplayComponent(
        wrongSource.dom.window as unknown as Window,
        componentRegistration()
      )
    ).toBe(false);

    const detached = make();
    detached.script.remove();
    Object.defineProperty(detached.dom.window, 'tsjs', {
      configurable: true,
      value: Object.defineProperty({}, '_registerFirstDisplay', {
        value: () => true,
      }),
    });
    expect(
      registerFirstDisplayComponent(
        detached.dom.window as unknown as Window,
        componentRegistration()
      )
    ).toBe(false);
  });

  it('owns the initial Didomi SDK path without clobbering publisher configuration', () => {
    const publisherConfig = { notice: 'publisher-owned' };
    const target = {
      didomiConfig: publisherConfig,
      location: { origin: 'https://publisher.example' },
    };
    const observations: Array<readonly [string, string]> = [];
    const disposers: Array<() => void> = [];
    const bindings = Object.freeze({
      config: Object.freeze({ proxyPath: '/integrations/didomi/consent/' }),
      observe: (name: string, value: string) => observations.push([name, value]),
      target,
    });
    const host = Object.freeze({
      activate: (
        id: string,
        own: (dispose: () => void) => void,
        install?: (candidate: unknown, ownEffect: (dispose: () => void) => void) => void
      ) => {
        expect(id).toBe('didomi_initial');
        install?.(bindings, own);
      },
    });
    const prepared = DIDOMI_INITIAL_SLICE.prepare(host);

    prepared.activate(
      Object.freeze({
        own: (dispose: () => void) => disposers.push(dispose),
        afterActivate: () => undefined,
      })
    );
    expect(target.didomiConfig).toBe(publisherConfig);
    expect(target.didomiConfig).toEqual({
      notice: 'publisher-owned',
      sdkPath: 'https://publisher.example/integrations/didomi/consent/',
    });
    expect(observations).toEqual([
      ['sdk_path', 'https://publisher.example/integrations/didomi/consent/'],
    ]);

    disposers.reverse().forEach((dispose) => dispose());
    expect(target.didomiConfig).toEqual({ notice: 'publisher-owned' });
  });

  it('keeps the Didomi initial config/rollback corpus equal to the persistent owner', () => {
    const validCases = [
      { config: Object.freeze({ proxyPath: '/consent/' }), initial: undefined },
      {
        config: Object.freeze({ proxyPath: '/consent/sdk/' }),
        initial: { notice: 'publisher' },
      },
      {
        config: Object.freeze({ proxyPath: '/consent/sdk/' }),
        initial: { notice: 'publisher', sdkPath: 'https://publisher.example/original.js' },
      },
    ] as const;
    for (const fixture of validCases) {
      const persistentTarget = {
        ...(fixture.initial ? { didomiConfig: { ...fixture.initial } } : {}),
        location: { origin: 'https://publisher.example' },
      };
      const initialTarget = {
        ...(fixture.initial ? { didomiConfig: { ...fixture.initial } } : {}),
        location: { origin: 'https://publisher.example' },
      };
      const persistentDispose = createDidomiRuntime({
        started: () => undefined,
        target: persistentTarget,
      }).activate(fixture.config);
      const initialDisposers: Array<() => void> = [];
      installDidomiInitial(
        Object.freeze({
          config: fixture.config,
          observe: () => undefined,
          target: initialTarget,
        }),
        (dispose) => initialDisposers.push(dispose)
      );
      expect(initialTarget.didomiConfig).toEqual(persistentTarget.didomiConfig);

      persistentDispose();
      initialDisposers.reverse().forEach((dispose) => dispose());
      expect(initialTarget.didomiConfig).toEqual(persistentTarget.didomiConfig);
      expect(Object.prototype.hasOwnProperty.call(initialTarget, 'didomiConfig')).toBe(
        Object.prototype.hasOwnProperty.call(persistentTarget, 'didomiConfig')
      );
    }

    for (const config of [
      { proxyPath: '/not-frozen/' },
      Object.freeze({ proxyPath: '//attacker.example/sdk.js' }),
      Object.freeze({ proxyPath: '/consent/?publisher=1' }),
    ]) {
      const persistent = () =>
        createDidomiRuntime({
          started: () => undefined,
          target: { location: { origin: 'https://publisher.example' } },
        }).activate(config);
      const initial = () =>
        installDidomiInitial(
          Object.freeze({
            config,
            observe: () => undefined,
            target: { location: { origin: 'https://publisher.example' } },
          }),
          () => undefined
        );
      expect(persistent).toThrow();
      expect(initial).toThrow();
    }
  });

  it('captures preexisting and later Testlight callbacks once without draining user work itself', () => {
    const calls: string[] = [];
    const first = () => calls.push('first');
    const throwing = () => {
      calls.push('throwing');
      throw new Error('publisher callback failed');
    };
    const second = () => calls.push('second');
    const later = () => calls.push('later');
    const original = [first, 'invalid', throwing, second];
    const target = { testlight: { publisher: true, que: original } };
    const observations: number[] = [];
    const disposers: Array<() => void> = [];
    const host = Object.freeze({
      activate: (
        id: string,
        own: (dispose: () => void) => void,
        install?: (candidate: unknown, ownEffect: (dispose: () => void) => void) => void
      ) => {
        expect(id).toBe('testlight_initial');
        install?.(
          Object.freeze({
            enqueue: (callback: () => void) => callback(),
            observe: (_name: string, count: number) => observations.push(count),
            target,
          }),
          own
        );
      },
    });

    TESTLIGHT_INITIAL_SLICE.prepare(host).activate(
      Object.freeze({
        own: (dispose: () => void) => disposers.push(dispose),
        afterActivate: () => undefined,
      })
    );
    expect(calls).toEqual(['first', 'throwing', 'second']);
    expect(target.testlight.que).not.toBe(original);
    expect(target.testlight.que.push(later)).toBe(1);
    expect(calls).toEqual(['first', 'throwing', 'second', 'later']);
    expect(target.testlight.que).toHaveLength(0);
    expect(observations).toEqual([3, 4]);

    disposers.reverse().forEach((dispose) => dispose());
    expect(target.testlight).toEqual({ publisher: true, que: original });
  });
});
