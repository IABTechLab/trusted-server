import { describe, expect, it, vi } from 'vitest';
import { JSDOM } from 'jsdom';

import {
  APS_INITIAL_SLICE,
  CREATIVE_INITIAL_SLICE,
  DATADOME_INITIAL_SLICE,
  DIDOMI_INITIAL_SLICE,
  GOOGLE_TAG_MANAGER_INITIAL_SLICE,
  GPT_INITIAL_SLICE,
  INITIAL_SLICE_DEFINITIONS,
  LOCKR_INITIAL_SLICE,
  OSANO_INITIAL_SLICE,
  PERMUTIVE_INITIAL_SLICE,
  PREBID_INITIAL_SLICE,
  selectInitialSliceDefinitions,
  SOURCEPOINT_INITIAL_SLICE,
  TESTLIGHT_INITIAL_SLICE,
} from '../../src/first_display/composition';
import type {
  FirstDisplaySliceHost,
  InitialSliceDefinition,
  InitialSliceInstaller,
} from '../../src/first_display/slices/definition';
import type { FirstDisplaySliceActivationContext } from '../../src/shared/first_display_transaction';
import type { FirstDisplayRouteRuleV1 } from '../../src/first_display/leaf/route_guard';
import { installDidomiInitial } from '../../src/first_display/leaf/config_guard';
import {
  installApsInitial,
  type FirstDisplayApsProtocolV1,
} from '../../src/first_display/leaf/aps_protocol';
import {
  installGptInitial,
  type FirstDisplayGptBatchPolicyV1,
  type FirstDisplayGptProtocolV1,
} from '../../src/first_display/leaf/gpt_protocol';
import {
  installPrebidInitial,
  type FirstDisplayPrebidProtocolV1,
} from '../../src/first_display/leaf/prebid_protocol';
import {
  installPermutiveInitial,
  snapshotPermutiveInitialSegments,
  type FirstDisplayContextRouteRuleV1,
} from '../../src/first_display/leaf/context_snapshot';
import {
  installOsanoInitial,
  installSourcepointInitial,
  type FirstDisplayConsentRouteRuleV1,
} from '../../src/first_display/leaf/consent_snapshot';
import {
  captureMutationObservedBindings,
  createFirstDisplayParserStateCollector,
  registerFirstDisplayComponent,
  type FirstDisplayComponentRegistrationV1,
} from '../../src/shared/first_display_registration';
import { createDidomiRuntime } from '../../src/integrations/didomi/module';
import { getPermutiveSegments } from '../../src/integrations/permutive/segments';
import { mirrorSourcepointConsent } from '../../src/integrations/sourcepoint/consent_mirror';
import {
  installRenderOwnerInitial,
  type FirstDisplayRenderOwnerProtocolV1,
} from '../../src/first_display/render_journal';

const RELEASE_ID = 'a'.repeat(64);

function clearAllCookies(): void {
  for (const cookie of document.cookie.split(';')) {
    const name = cookie.split('=')[0]?.trim();
    if (name) document.cookie = `${name}=; path=/; Max-Age=0`;
  }
}

function readCookie(name: string): string | undefined {
  const prefix = `${name}=`;
  return document.cookie
    .split('; ')
    .find((entry) => entry.startsWith(prefix))
    ?.slice(prefix.length);
}

function componentRegistration(): FirstDisplayComponentRegistrationV1 {
  return Object.freeze({
    abi: 1,
    id: 'gpt_initial',
    releaseId: RELEASE_ID,
    order: 7,
    install: () => undefined,
  });
}

function activateInitialSlice(
  definition: InitialSliceDefinition,
  host: FirstDisplaySliceHost,
  context: FirstDisplaySliceActivationContext
): void {
  host.activate(definition.id, context.own, definition.install);
}

describe('first-display initial slice definitions', () => {
  it('captures bounded parser observations once per key in canonical slice order', () => {
    const collector = createFirstDisplayParserStateCollector();

    expect(collector.register('lockr_initial')).toBe(true);
    expect(collector.observe('gpt_initial', 'protocol_version', 1)).toBe(true);
    expect(collector.observe('creative_initial', 'guard_count', 1)).toBe(true);
    expect(collector.observe('gpt_initial', 'protocol_version', 2)).toBe(true);
    expect(collector.observe('gpt_initial', '', 3)).toBe(false);

    const snapshot = collector.snapshot();
    expect(snapshot).toEqual([
      {
        sliceId: 'creative_initial',
        observations: ['guard_count'],
        values: [['guard_count', 1]],
      },
      {
        sliceId: 'gpt_initial',
        observations: ['protocol_version'],
        values: [['protocol_version', 2]],
      },
      { sliceId: 'lockr_initial', observations: [], values: [] },
    ]);
    expect(Object.isFrozen(snapshot)).toBe(true);
    expect(Object.isFrozen(snapshot[0]?.values[0])).toBe(true);
  });

  it('preserves exact frozen bindings while observing each successful parser update', () => {
    const events: string[] = [];
    const candidate = Object.freeze({
      observe: (name: string, value: unknown) => events.push(`observe:${name}:${String(value)}`),
      register: () => () => undefined,
    });
    const observations: unknown[] = [];
    const captured = captureMutationObservedBindings(
      candidate,
      () => {
        events.push('mutation');
        return true;
      },
      (key, value) => observations.push([key, value])
    ) as typeof candidate;

    expect(captured).not.toBe(candidate);
    expect(Object.isFrozen(captured)).toBe(true);
    expect(Reflect.ownKeys(captured)).toEqual(['observe', 'register']);
    captured.observe('segment_count', 2);
    expect(events).toEqual(['observe:segment_count:2', 'mutation']);
    expect(observations).toEqual([['segment_count', 2]]);
  });

  it('returns exact protocol activation receipts while registering full protocols', () => {
    const own = vi.fn();
    const register = vi.fn((_protocol: object) => () => undefined);
    const observe = vi.fn();

    const receipts = [
      installApsInitial(
        Object.freeze({
          observe,
          publisherOrigin: 'https://publisher.example',
          register,
        }),
        own
      ),
      installGptInitial(
        Object.freeze({ browser: {} as Window, observe, register }),
        own,
        () =>
          Object.freeze({
            start: () => true,
            closeIngress: () => true,
            captureHandoff: () => Object.freeze([]),
            captureDiagnosticsHandoff: () =>
              Object.freeze([Object.freeze([]), Object.freeze([]), 1, 0, 0] as const),
            detachCommittedSlots: () => true,
            dispose: () => undefined,
          }),
        Object.freeze({ gamAttributionEnabled: false, pageBidsEnabled: true })
      ),
      installPrebidInitial(Object.freeze({ observe, register }), own),
    ];

    expect(receipts).toEqual([
      [1, 'aps'],
      [1, 'gpt'],
      [1, 'prebid'],
    ]);
    for (const receipt of receipts) {
      expect(Reflect.ownKeys(receipt)).toEqual(['0', '1', 'length']);
      expect(Object.isFrozen(receipt)).toBe(true);
    }
    expect(register).toHaveBeenCalledTimes(3);
    for (const [protocol] of register.mock.calls) {
      expect(Reflect.ownKeys(protocol).length).toBeGreaterThan(2);
      expect(Object.isFrozen(protocol)).toBe(true);
    }
  });

  it('installs one source-neutral render journal and gives its slice disposer final ownership', () => {
    const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
      url: 'https://publisher.example/',
    });
    const removeEventListener = vi.spyOn(dom.window, 'removeEventListener');
    const release = vi.fn();
    const owned: Array<() => void> = [];
    let protocol: FirstDisplayRenderOwnerProtocolV1 | undefined;

    expect(
      installRenderOwnerInitial(
        Object.freeze({
          observe: vi.fn(),
          register: (candidate: FirstDisplayRenderOwnerProtocolV1) => {
            protocol = candidate;
            return release;
          },
        }),
        (dispose) => owned.push(dispose)
      )
    ).toEqual([1, 'render_owner']);
    expect(protocol).toEqual([1, 'render_owner', expect.any(Function)]);
    expect(Object.isFrozen(protocol)).toBe(true);
    const createRenderBridge = protocol?.[2];
    const bridge = createRenderBridge?.([
      dom.window as unknown as Window,
      () => undefined,
      () => {
        throw new Error('unused');
      },
      dom.window.document,
      () => undefined,
      () => 0,
      undefined,
      () => ({}),
    ]);

    expect(bridge).toHaveLength(10);
    expect(Object.isFrozen(bridge)).toBe(true);
    expect(() =>
      createRenderBridge?.([
        dom.window as unknown as Window,
        () => undefined,
        () => {
          throw new Error('unused');
        },
        dom.window.document,
        () => undefined,
        () => 0,
        undefined,
        () => ({}),
      ])
    ).toThrow(TypeError);

    expect(owned).toHaveLength(1);
    owned[0]?.();
    expect(release).toHaveBeenCalledOnce();
    expect(removeEventListener).toHaveBeenCalledWith('message', expect.any(Function), true);
    dom.window.close();
  });

  it.each([false, true])(
    'projects the typed GAM attribution flag into the first-display GPT owner (%s)',
    (gamAttributionEnabled) => {
      const observe = vi.fn();
      const commands: Array<() => void> = [];
      const setConfig = vi.fn();
      const browser = {
        googletag: { cmd: commands, setConfig },
      } as unknown as Window;
      let protocol: FirstDisplayGptProtocolV1 | undefined;
      let policy: FirstDisplayGptBatchPolicyV1 | undefined;
      const createBatch = vi.fn((_input: unknown, candidate: FirstDisplayGptBatchPolicyV1) => {
        policy = candidate;
        return Object.freeze({
          start: () => true,
          closeIngress: () => true,
          captureHandoff: () => Object.freeze([]),
          captureDiagnosticsHandoff: () =>
            Object.freeze([Object.freeze([]), Object.freeze([]), 1, 0, 0] as const),
          detachCommittedSlots: () => true,
          dispose: () => undefined,
        });
      });
      installGptInitial(
        Object.freeze({
          browser,
          observe,
          register: (candidate: FirstDisplayGptProtocolV1) => {
            protocol = candidate;
            return () => undefined;
          },
        }),
        () => undefined,
        createBatch,
        Object.freeze({ gamAttributionEnabled, pageBidsEnabled: true })
      );

      expect(protocol).toEqual([1, 'gpt', expect.any(Function)]);
      expect(Object.isFrozen(protocol)).toBe(true);
      const batch = protocol?.[2]({} as never);
      expect(batch).toHaveLength(6);
      expect(Object.isFrozen(batch)).toBe(true);

      expect(createBatch).toHaveBeenCalledOnce();
      expect(createBatch.mock.calls[0]?.[0]).toEqual({});
      expect(policy?.deadlines).toEqual({
        externalReadyMs: 10_000,
        requestStartMs: 3_000,
        completionMs: 10_000,
      });
      expect(
        policy?.requestPlan(
          Object.freeze({ initialLoadDisabled: true, ownership: 'trusted_server' })
        )
      ).toEqual({ operations: ['display', 'refresh'], requestOperation: 1 });
      expect(policy?.classifyRenderEnded(Object.freeze({ isEmpty: false }))).toBe('nonempty_gam');
      expect(commands).toHaveLength(gamAttributionEnabled ? 1 : 0);
      commands.splice(0).forEach((command) => command());
      expect(setConfig).toHaveBeenCalledTimes(gamAttributionEnabled ? 1 : 0);
      if (gamAttributionEnabled) {
        expect(setConfig).toHaveBeenCalledExactlyOnceWith({ targeting: { ts: 'true' } });
      }
      expect(observe).toHaveBeenCalledWith('gam', gamAttributionEnabled);
    }
  );

  it('pins the exact thirteen optional slices in build order', () => {
    expect(INITIAL_SLICE_DEFINITIONS.map(({ id }) => id)).toEqual([
      'render_owner_initial',
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

  it('exposes only each exact initial-slice installer obligation', () => {
    const events: string[] = [];
    const dispose = vi.fn();
    const host = Object.freeze({
      activate: (
        id: string,
        own: (callback: () => void) => void,
        _install: InitialSliceInstaller
      ) => {
        own(dispose);
        events.push(id);
      },
    });

    for (const definition of INITIAL_SLICE_DEFINITIONS) {
      host.activate(definition.id, () => undefined, definition.install);
    }
    expect(events).toEqual(INITIAL_SLICE_DEFINITIONS.map(({ id }) => id));
    expect(
      INITIAL_SLICE_DEFINITIONS.every(
        (definition) => Reflect.ownKeys(definition).join(',') === 'id,install'
      )
    ).toBe(true);
  });

  it('rejects unknown, duplicate, omitted-base, and misordered selections', () => {
    expect(
      selectInitialSliceDefinitions(['first_display', 'gpt_initial'])?.map(({ id }) => id)
    ).toEqual(['gpt_initial']);
    expect(selectInitialSliceDefinitions(['gpt_initial'])).toBeUndefined();
    expect(
      selectInitialSliceDefinitions(['first_display', 'gpt_initial', 'gpt_initial'])
    ).toBeUndefined();
    expect(
      selectInitialSliceDefinitions(['first_display', 'prebid_initial', 'gpt_initial'])
    ).toBeUndefined();
    expect(
      selectInitialSliceDefinitions(['first_display', 'aps_initial', 'gpt_initial'])
    ).toBeUndefined();
    expect(
      selectInitialSliceDefinitions(['first_display', 'render_owner_initial'])
    ).toBeUndefined();
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
      registerFirstDisplayComponent(dom.window as unknown as Window, componentRegistrationValue)
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
        install?: InitialSliceInstaller
      ) => {
        expect(id).toBe('didomi_initial');
        install?.(bindings, own, undefined);
      },
    });
    activateInitialSlice(
      DIDOMI_INITIAL_SLICE,
      host,
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
        install?: InitialSliceInstaller
      ) => {
        expect(id).toBe('testlight_initial');
        install?.(
          Object.freeze({
            enqueue: (callback: () => void) => callback(),
            observe: (_name: string, count: number) => observations.push(count),
            target,
          }),
          own,
          undefined
        );
      },
    });

    activateInitialSlice(
      TESTLIGHT_INITIAL_SLICE,
      host,
      Object.freeze({
        own: (dispose: () => void) => disposers.push(dispose),
        afterActivate: () => undefined,
      })
    );
    expect(calls).toEqual(['first', 'throwing', 'second']);
    expect(original).toEqual([]);
    expect(target.testlight.que).not.toBe(original);
    expect(target.testlight.que.push(later)).toBe(1);
    expect(calls).toEqual(['first', 'throwing', 'second', 'later']);
    expect(target.testlight.que).toHaveLength(0);
    expect(observations).toEqual([3, 4]);

    disposers.reverse().forEach((dispose) => dispose());
    expect(target.testlight).toEqual({ publisher: true, que: original });
    expect(original).toEqual([]);
  });

  it('registers exact DataDome and GTM route matchers with first-party path preservation', () => {
    const cases = [
      {
        definition: DATADOME_INITIAL_SLICE,
        id: 'datadome',
        accepted: [
          ['script', 'https://js.datadome.co/tags.js?x=1'],
          ['preload', '//js.datadome.co/js/check'],
        ],
        rejected: [
          ['script', 'https://cdn.example/js.datadome.co.js'],
          ['beacon', 'https://js.datadome.co/tags.js'],
        ],
        rewritten: 'https://publisher.example/integrations/datadome/tags.js?x=1',
      },
      {
        definition: GOOGLE_TAG_MANAGER_INITIAL_SLICE,
        id: 'google_tag_manager',
        accepted: [
          ['script', 'https://www.googletagmanager.com/gtm.js?id=GTM-1'],
          ['fetch', 'https://www.google-analytics.com/g/collect?v=2'],
          ['beacon', 'https://analytics.google.com/collect?v=2'],
        ],
        rejected: [
          ['script', 'https://googletagmanager.com/gtm.js'],
          ['script', 'https://www.googletagmanager.com/ns.html'],
        ],
        rewritten: 'https://publisher.example/integrations/google_tag_manager/gtm.js?id=GTM-1',
      },
    ] as const;

    for (const fixture of cases) {
      let rule: FirstDisplayRouteRuleV1 | undefined;
      const dispose = vi.fn();
      const disposers: Array<() => void> = [];
      const host = Object.freeze({
        activate: (
          _id: string,
          own: (release: () => void) => void,
          install?: InitialSliceInstaller
        ) =>
          install?.(
            Object.freeze({
              observe: () => undefined,
              origin: 'https://publisher.example',
              register: (candidate: FirstDisplayRouteRuleV1) => {
                rule = candidate;
                return dispose;
              },
            }),
            own,
            undefined
          ),
      });
      activateInitialSlice(
        fixture.definition,
        host,
        Object.freeze({
          own: (release: () => void) => disposers.push(release),
          afterActivate: () => undefined,
        })
      );
      expect(rule?.id).toBe(fixture.id);
      for (const [kind, url] of fixture.accepted) expect(rule?.matches(kind, url)).toBe(true);
      for (const [kind, url] of fixture.rejected) expect(rule?.matches(kind, url)).toBe(false);
      expect(rule?.rewrite(fixture.accepted[0][1])).toBe(fixture.rewritten);
      disposers.reverse().forEach((release) => release());
      expect(dispose).toHaveBeenCalledOnce();
    }
  });

  it('owns Lockr route matching and the bounded initial SDK host rewrite', () => {
    const sdk = { host: 'https://identity.loc.kr' };
    const timers: Array<() => void> = [];
    const disposers: Array<() => void> = [];
    let rule: FirstDisplayRouteRuleV1 | undefined;
    const unregister = vi.fn();
    const observations: Array<readonly [string, string | number]> = [];
    const bindings = Object.freeze({
      clearTimer: (handle: unknown) => {
        const index = timers.indexOf(handle as () => void);
        if (index >= 0) timers.splice(index, 1);
      },
      getSdk: () => sdk,
      host: 'publisher.example',
      observe: (name: string, value: string | number) => observations.push([name, value]),
      origin: 'https://publisher.example',
      protocol: 'https:',
      register: (candidate: FirstDisplayRouteRuleV1) => {
        rule = candidate;
        return unregister;
      },
      setTimer: (callback: () => void) => {
        timers.push(callback);
        return callback;
      },
    });
    const host = Object.freeze({
      activate: (
        _id: string,
        own: (release: () => void) => void,
        install?: InitialSliceInstaller
      ) => install?.(bindings, own, undefined),
    });

    activateInitialSlice(
      LOCKR_INITIAL_SLICE,
      host,
      Object.freeze({
        own: (release: () => void) => disposers.push(release),
        afterActivate: () => undefined,
      })
    );
    expect(rule?.matches('script', 'https://aim.loc.kr/sdk.js')).toBe(true);
    expect(rule?.matches('preload', 'https://identity.loc.kr/identity-lockr.js')).toBe(true);
    expect(rule?.matches('script', 'https://identity.loc.kr/other.js')).toBe(false);
    expect(rule?.rewrite('https://aim.loc.kr/sdk.js')).toBe(
      'https://publisher.example/integrations/lockr/sdk'
    );
    expect(sdk.host).toBe('https://publisher.example/integrations/lockr/api');
    expect(observations).toContainEqual(['sdk_host', sdk.host]);

    disposers.reverse().forEach((release) => release());
    expect(sdk.host).toBe('https://identity.loc.kr');
    expect(unregister).toHaveBeenCalledOnce();
    expect(timers).toEqual([]);
  });

  it('captures Permutive segments and owns its initial route/readiness mutations', () => {
    localStorage.setItem(
      'permutive-app',
      JSON.stringify({ core: { cohorts: { all: ['one', 2, false, ...Array(110).fill('x')] } } })
    );
    const sdk = {
      config: {
        apiHost: 'api.permutive.com',
        apiProtocol: 'https',
        cdnBaseUrl: 'cdn.permutive.com',
        cdnProtocol: 'https',
        secureSignalsApiHost: 'signals.permutive.com',
        segmentSyncApiHost: 'sync.permutive.com',
      },
    };
    const previous = { ...sdk.config };
    const disposers: Array<() => void> = [];
    let route: FirstDisplayContextRouteRuleV1 | undefined;
    const unregisterRoute = vi.fn();
    const observations: Array<readonly [string, string | number]> = [];
    const bindings = Object.freeze({
      clearTimer: () => undefined,
      getSdk: () => sdk,
      host: 'publisher.example',
      observe: (name: string, value: string | number) => observations.push([name, value]),
      origin: 'https://publisher.example',
      protocol: 'https:',
      readStorage: (key: string) => localStorage.getItem(key),
      registerRoute: (candidate: FirstDisplayContextRouteRuleV1) => {
        route = candidate;
        return unregisterRoute;
      },
      setTimer: (_callback: () => void, _delayMs: number) => 1,
    });
    const host = Object.freeze({
      activate: (
        id: string,
        own: (release: () => void) => void,
        install?: InitialSliceInstaller
      ) => {
        expect(id).toBe('permutive_initial');
        install?.(bindings, own, undefined);
      },
    });

    activateInitialSlice(
      PERMUTIVE_INITIAL_SLICE,
      host,
      Object.freeze({
        own: (release: () => void) => disposers.push(release),
        afterActivate: () => undefined,
      })
    );

    const segmentObservation = observations.find(([name]) => name === 'segments');
    expect(segmentObservation).toBeDefined();
    expect(JSON.parse(String(segmentObservation?.[1]))).toEqual(getPermutiveSegments());
    expect(JSON.parse(String(segmentObservation?.[1]))).toHaveLength(100);
    expect(route?.matches('script', 'https://cdn.permutive.com/example-web.js')).toBe(true);
    expect(
      route?.matches('script', 'https://cdn.permutive.com.attacker.example/example-web.js')
    ).toBe(false);
    expect(
      route?.matches('script', 'https://cdn.permutive.com@attacker.example/example-web.js')
    ).toBe(false);
    expect(route?.matches('script', 'https://cdn.permutive.com/example.js')).toBe(false);
    expect(route?.rewrite('https://cdn.permutive.com/example-web.js')).toBe(
      'https://publisher.example/integrations/permutive/sdk'
    );
    expect(sdk.config).toEqual({
      apiHost: 'publisher.example/integrations/permutive/api',
      apiProtocol: 'https',
      cdnBaseUrl: 'publisher.example/integrations/permutive/cdn',
      cdnProtocol: 'https',
      secureSignalsApiHost: 'publisher.example/integrations/permutive/secure-signal',
      segmentSyncApiHost: 'publisher.example/integrations/permutive/sync',
    });

    disposers.reverse().forEach((release) => release());
    expect(sdk.config).toEqual(previous);
    expect(unregisterRoute).toHaveBeenCalledOnce();
    localStorage.clear();
  });

  it('mirrors only the one-shot Sourcepoint consent snapshot and owns its SDK route', () => {
    clearAllCookies();
    localStorage.clear();
    localStorage.setItem(
      '_sp_user_consent_123',
      JSON.stringify({
        gppData: { gppString: 'DBABLA~BVQqAAAAAgA.QA', applicableSections: [7, 8] },
      })
    );
    expect(mirrorSourcepointConsent()).toBe(true);
    const persistentCookies = document.cookie;
    clearAllCookies();

    const disposers: Array<() => void> = [];
    let route: FirstDisplayConsentRouteRuleV1 | undefined;
    const unregisterRoute = vi.fn();
    const bindings = Object.freeze({
      config: Object.freeze({ rewriteSdk: true }),
      document,
      observe: () => undefined,
      origin: 'https://publisher.example',
      registerRoute: (candidate: FirstDisplayConsentRouteRuleV1) => {
        route = candidate;
        return unregisterRoute;
      },
      storage: localStorage,
    });
    const host = Object.freeze({
      activate: (
        id: string,
        own: (release: () => void) => void,
        install?: InitialSliceInstaller
      ) => {
        expect(id).toBe('sourcepoint_initial');
        install?.(bindings, own, undefined);
      },
    });

    activateInitialSlice(
      SOURCEPOINT_INITIAL_SLICE,
      host,
      Object.freeze({
        own: (release: () => void) => disposers.push(release),
        afterActivate: () => undefined,
      })
    );
    expect(document.cookie).toBe(persistentCookies);
    expect(
      route?.matches('script', 'https://cdn.privacy-mgmt.com/wrapperMessagingWithoutDetection.js')
    ).toBe(true);
    expect(route?.matches('script', 'https://cdn.privacy-mgmt.com.evil.example/sdk.js')).toBe(
      false
    );
    expect(route?.rewrite('https://cdn.privacy-mgmt.com/path/sdk.js?x=1')).toBe(
      'https://publisher.example/integrations/sourcepoint/cdn/path/sdk.js?x=1'
    );

    disposers.reverse().forEach((release) => release());
    expect(readCookie('__gpp')).toBeUndefined();
    expect(readCookie('__gpp_sid')).toBeUndefined();
    expect(readCookie('_ts_gpp_src')).toBeUndefined();
    expect(unregisterRoute).toHaveBeenCalledOnce();
    localStorage.clear();
    clearAllCookies();
  });

  it('captures one Osano USP/GPP/TCF snapshot without installing maintenance listeners', () => {
    clearAllCookies();
    const disposers: Array<() => void> = [];
    const timers = new Set<() => void>();
    const target = {
      addEventListener: vi.fn(),
      __uspapi: (
        _command: string,
        _version: number,
        callback: (data: unknown, success: boolean) => void
      ) => callback({ uspString: '1YN-' }, true),
      __gpp: (_command: string, callback: (data: unknown, success: boolean) => void) =>
        callback(
          { signalStatus: 'ready', gppString: 'DBABLA~BVQqAAAAAgA.QA', applicableSections: [7] },
          true
        ),
      __tcfapi: (
        _command: string,
        _version: number,
        callback: (data: unknown, success: boolean) => void
      ) => callback({ eventStatus: 'tcloaded', tcString: 'consent-string' }, true),
    };
    const bindings = Object.freeze({
      clearTimer: (handle: unknown) => timers.delete(handle as () => void),
      document,
      observe: () => undefined,
      setTimer: (callback: () => void, _delayMs: number) => {
        timers.add(callback);
        return callback;
      },
      target,
    });
    const host = Object.freeze({
      activate: (
        id: string,
        own: (release: () => void) => void,
        install?: InitialSliceInstaller
      ) => {
        expect(id).toBe('osano_initial');
        install?.(bindings, own, undefined);
      },
    });

    activateInitialSlice(
      OSANO_INITIAL_SLICE,
      host,
      Object.freeze({
        own: (release: () => void) => disposers.push(release),
        afterActivate: () => undefined,
      })
    );

    expect(readCookie('us_privacy')).toBe('1YN-');
    expect(readCookie('__gpp')).toBe('DBABLA~BVQqAAAAAgA.QA');
    expect(readCookie('__gpp_sid')).toBe('7');
    expect(readCookie('euconsent-v2')).toBe('consent-string');
    expect(readCookie('_ts_consent_src')).toBe('osano');
    expect(target.addEventListener).not.toHaveBeenCalled();
    expect(timers.size).toBe(0);

    disposers.reverse().forEach((release) => release());
    expect(document.cookie).toBe('');
  });

  it('keeps Permutive fallback parsing bounded and releases a pending readiness check', () => {
    const raw = JSON.stringify({
      eventPublication: {
        eventUpload: [
          ['old', { event: { properties: { segments: ['old'] } } }],
          ['new', { event: { properties: { segments: [9, 'new', false] } } }],
        ],
      },
    });
    expect(snapshotPermutiveInitialSegments(raw)).toEqual(['9', 'new']);
    expect(snapshotPermutiveInitialSegments('{bad-json')).toEqual([]);

    const timers: Array<() => void> = [];
    const disposers: Array<() => void> = [];
    const unregisterRoute = vi.fn();
    installPermutiveInitial(
      Object.freeze({
        clearTimer: (handle: unknown) => {
          const index = timers.indexOf(handle as () => void);
          if (index >= 0) timers.splice(index, 1);
        },
        getSdk: () => undefined,
        host: 'publisher.example',
        observe: () => undefined,
        origin: 'https://publisher.example',
        protocol: 'https:',
        readStorage: () => raw,
        registerRoute: () => unregisterRoute,
        setTimer: (callback: () => void) => {
          timers.push(callback);
          return callback;
        },
      }),
      (release) => disposers.push(release)
    );
    expect(timers).toHaveLength(1);

    disposers.reverse().forEach((release) => release());
    expect(timers).toEqual([]);
    expect(unregisterRoute).toHaveBeenCalledOnce();
  });

  it('preserves a foreign GPP owner and skips the disabled Sourcepoint SDK route', () => {
    clearAllCookies();
    localStorage.clear();
    document.cookie = '__gpp=publisher-owned; path=/';
    document.cookie = '__gpp_sid=2; path=/';
    localStorage.setItem(
      '_sp_user_consent_123',
      JSON.stringify({ gppData: { gppString: 'sourcepoint', applicableSections: [7] } })
    );
    const registerRoute = vi.fn();
    const disposers: Array<() => void> = [];
    installSourcepointInitial(
      Object.freeze({
        config: Object.freeze({ rewriteSdk: false }),
        document,
        observe: () => undefined,
        origin: 'https://publisher.example',
        registerRoute,
        storage: localStorage,
      }),
      (release) => disposers.push(release)
    );

    expect(readCookie('__gpp')).toBe('publisher-owned');
    expect(readCookie('__gpp_sid')).toBe('2');
    expect(readCookie('_ts_gpp_src')).toBeUndefined();
    expect(registerRoute).not.toHaveBeenCalled();
    disposers.reverse().forEach((release) => release());
    expect(readCookie('__gpp')).toBe('publisher-owned');
    clearAllCookies();
    localStorage.clear();
  });

  it('cancels a pending Osano API snapshot without later work or cookie mutation', () => {
    clearAllCookies();
    const timers = new Set<() => void>();
    const disposers: Array<() => void> = [];
    const target = {
      addEventListener: vi.fn(),
      __uspapi: vi.fn(),
    };
    installOsanoInitial(
      Object.freeze({
        clearTimer: (handle: unknown) => timers.delete(handle as () => void),
        document,
        observe: () => undefined,
        setTimer: (callback: () => void) => {
          timers.add(callback);
          return callback;
        },
        target,
      }),
      (release) => disposers.push(release)
    );
    expect(timers.size).toBe(1);
    expect(target.addEventListener).not.toHaveBeenCalled();

    disposers.reverse().forEach((release) => release());
    expect(timers.size).toBe(0);
    expect(document.cookie).toBe('');
  });

  it('installs the selected creative parser guards and owns their rollback', () => {
    const observations: Array<readonly [string, number]> = [];
    const disposers: Array<() => void> = [];
    const clickHandle = Object.freeze({ dispose: vi.fn(), scan: vi.fn() });
    const imageHandle = Object.freeze({ dispose: vi.fn(), scan: vi.fn() });
    const iframeHandle = Object.freeze({ dispose: vi.fn(), scan: vi.fn() });
    const installClickGuard = vi.fn(() => clickHandle);
    const installDynamicImageProxy = vi.fn(() => imageHandle);
    const installDynamicIframeProxy = vi.fn(() => iframeHandle);
    const bindings = Object.freeze({
      config: Object.freeze({
        version: 1,
        enabled: true,
        clickGuard: true,
        renderGuard: true,
      }),
      document,
      installClickGuard,
      installDynamicIframeProxy,
      installDynamicImageProxy,
      observe: (name: string, value: number) => observations.push([name, value]),
    });
    const host = Object.freeze({
      activate: (
        id: string,
        own: (dispose: () => void) => void,
        install?: InitialSliceInstaller
      ) => {
        expect(id).toBe('creative_initial');
        install?.(bindings, own, bindings.config);
      },
    });

    activateInitialSlice(
      CREATIVE_INITIAL_SLICE,
      host,
      Object.freeze({
        own: (dispose: () => void) => disposers.push(dispose),
        afterActivate: () => undefined,
      })
    );
    expect(installClickGuard).toHaveBeenCalledOnce();
    expect(installDynamicImageProxy).toHaveBeenCalledOnce();
    expect(installDynamicIframeProxy).toHaveBeenCalledOnce();
    expect(clickHandle.scan).not.toHaveBeenCalled();
    expect(imageHandle.scan).not.toHaveBeenCalled();
    expect(iframeHandle.scan).not.toHaveBeenCalled();
    expect(observations).toEqual([['guard_count', 3]]);

    disposers.reverse().forEach((dispose) => dispose());
    expect(clickHandle.dispose).toHaveBeenCalledOnce();
    expect(imageHandle.dispose).toHaveBeenCalledOnce();
    expect(iframeHandle.dispose).toHaveBeenCalledOnce();
  });

  it('rejects malformed creative config before installing any initial guard', () => {
    for (const config of [
      { version: 1, enabled: true, clickGuard: true, renderGuard: false },
      Object.freeze({ version: 1, enabled: false, clickGuard: true, renderGuard: false }),
      Object.freeze({
        version: 1,
        enabled: true,
        clickGuard: true,
        renderGuard: false,
        extra: true,
      }),
    ]) {
      const installClickGuard = vi.fn();
      const host = Object.freeze({
        activate: (
          _id: string,
          own: (dispose: () => void) => void,
          install?: InitialSliceInstaller
        ) =>
          install?.(
            Object.freeze({
              config,
              document,
              installClickGuard,
              installDynamicIframeProxy: vi.fn(),
              installDynamicImageProxy: vi.fn(),
              observe: () => undefined,
            }),
            own,
            config
          ),
      });
      expect(() =>
        activateInitialSlice(
          CREATIVE_INITIAL_SLICE,
          host,
          Object.freeze({ own: () => undefined, afterActivate: () => undefined })
        )
      ).toThrow();
      expect(installClickGuard).not.toHaveBeenCalled();
    }
  });

  it('registers the closed APS nonce, policy, and document-channel protocol', () => {
    const release = vi.fn();
    const disposers: Array<() => void> = [];
    let protocol: FirstDisplayApsProtocolV1 | undefined;
    const bindings = Object.freeze({
      observe: vi.fn(),
      publisherOrigin: 'https://publisher.example',
      register: (candidate: FirstDisplayApsProtocolV1) => {
        protocol = candidate;
        return release;
      },
    });
    const host = Object.freeze({
      activate: (
        id: string,
        own: (dispose: () => void) => void,
        install?: InitialSliceInstaller
      ) => {
        expect(id).toBe('aps_initial');
        install?.(bindings, own, undefined);
      },
    });

    activateInitialSlice(
      APS_INITIAL_SLICE,
      host,
      Object.freeze({
        own: (dispose: () => void) => disposers.push(dispose),
        afterActivate: () => undefined,
      })
    );
    const policy = protocol?.[2];
    expect(policy?.rendererUrl).toBe('https://publisher.example/integrations/aps/renderer/v2');
    expect(policy?.publisherOrigin).toBe('https://publisher.example');
    expect(policy?.sandbox).toBe(
      'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation'
    );
    expect(policy?.permanentSandbox).toBe(
      'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-same-origin allow-scripts allow-top-navigation-by-user-activation'
    );
    expect(policy?.deadlines).toEqual({
      documentAcceptanceMs: 3_000,
      completionMs: 10_000,
    });
    expect(policy?.isBootstrapNonce(`b1_${'a'.repeat(22)}`)).toBe(true);
    expect(policy?.isRendererNonce(`n1_${'a'.repeat(22)}`)).toBe(true);
    expect(policy?.isBootstrapNonce(`n1_${'a'.repeat(22)}`)).toBe(false);
    const nonce = `n1_${'b'.repeat(22)}`;
    expect(
      policy?.parseDocumentMessage(
        Object.freeze({ message: 'TS APS Document Accepted', version: 1, nonce }),
        nonce
      )
    ).toEqual({ kind: 'document_accepted' });
    expect(
      policy?.parseDocumentMessage(
        Object.freeze({ message: 'TS APS Runner Loaded', version: 1, nonce }),
        nonce
      )
    ).toEqual({ kind: 'runner_loaded' });
    expect(
      policy?.parseDocumentMessage(
        Object.freeze({ message: 'TS APS Render Completed', version: 1, nonce }),
        nonce
      )
    ).toEqual({ kind: 'render_completed' });
    expect(
      policy?.parseDocumentMessage(
        Object.freeze({
          message: 'TS APS Render Failed',
          version: 1,
          nonce,
          reason: 'runner_failed',
        }),
        nonce
      )
    ).toEqual({ kind: 'render_failed', reason: 'runner_failed' });
    for (const message of [
      { message: 'TS APS Render Completed', version: 1, nonce: `n1_${'c'.repeat(22)}` },
      { message: 'TS APS Render Completed', version: 1, nonce, extra: true },
      { message: 'TS APS Render Failed', version: 1, nonce, reason: 'unknown' },
      Object.create({ message: 'TS APS Render Completed', version: 1, nonce }),
    ]) {
      expect(policy?.parseDocumentMessage(Object.freeze(message), nonce)).toBeUndefined();
    }

    disposers.reverse().forEach((dispose) => dispose());
    expect(release).toHaveBeenCalledOnce();
  });

  it('registers only the attenuated GPT batch factory before any initial action', () => {
    const release = vi.fn();
    const disposers: Array<() => void> = [];
    let protocol: FirstDisplayGptProtocolV1 | undefined;
    const bindings = Object.freeze({
      browser: {} as Window,
      observe: vi.fn(),
      register: (candidate: FirstDisplayGptProtocolV1) => {
        protocol = candidate;
        return release;
      },
    });
    const host = Object.freeze({
      activate: (
        id: string,
        own: (dispose: () => void) => void,
        install?: InitialSliceInstaller
      ) => {
        expect(id).toBe('gpt_initial');
        install?.(
          bindings,
          own,
          Object.freeze({ gamAttributionEnabled: false, pageBidsEnabled: true })
        );
      },
    });

    activateInitialSlice(
      GPT_INITIAL_SLICE,
      host,
      Object.freeze({
        own: (dispose: () => void) => disposers.push(dispose),
        afterActivate: () => undefined,
      })
    );
    expect(protocol).toEqual([1, 'gpt', expect.any(Function)]);
    expect(Object.isFrozen(protocol)).toBe(true);

    disposers.reverse().forEach((dispose) => dispose());
    expect(release).toHaveBeenCalledOnce();
  });

  it('registers the Prebid admission policy for the protected batch only', () => {
    const release = vi.fn();
    const disposers: Array<() => void> = [];
    let protocol: FirstDisplayPrebidProtocolV1 | undefined;
    const bindings = Object.freeze({
      observe: vi.fn(),
      register: (candidate: FirstDisplayPrebidProtocolV1) => {
        protocol = candidate;
        return release;
      },
    });
    const host = Object.freeze({
      activate: (
        id: string,
        own: (dispose: () => void) => void,
        install?: InitialSliceInstaller
      ) => {
        expect(id).toBe('prebid_initial');
        install?.(bindings, own, undefined);
      },
    });

    activateInitialSlice(
      PREBID_INITIAL_SLICE,
      host,
      Object.freeze({
        own: (dispose: () => void) => disposers.push(dispose),
        afterActivate: () => undefined,
      })
    );
    const policy = protocol?.[2];
    expect(policy).toMatchObject({
      bidderCode: 'trustedServer',
      maxPendingOperations: 64,
      externalReadyMs: 10_000,
      admissionLeaseMs: 10_000,
      renderReservationMs: 15 * 60 * 1_000,
    });
    expect(policy?.normalizeEidSource('  ID5-SYNC.COM ')).toBe('id5-sync.com');
    expect(policy?.normalizeEidSource('   ')).toBeUndefined();
    const prepared = policy?.snapshotTrustedBid(
      Object.freeze({
        auctionId: 'auction-1',
        adUnitCode: 'slot-1',
        bid: Object.freeze({
          requestId: 'request-1',
          adId: `r1_${'a'.repeat(22)}`,
          cpm: 1.25,
          width: 300,
          height: 250,
          ad: '',
          ttl: 300,
          creativeId: 'creative-1',
          netRevenue: true,
          currency: 'USD',
          bidderCode: 'trustedServer',
          meta: Object.freeze({
            advertiserDomains: Object.freeze(['advertiser.example']),
            tsAuctionId: 'auction-1',
            tsBidId: 'bid-1',
          }),
        }),
      })
    );
    expect(prepared).toBeDefined();
    expect(Object.isFrozen(prepared)).toBe(true);
    expect(prepared?.bid.adId).toBe(`r1_${'a'.repeat(22)}`);
    expect(
      policy?.snapshotTrustedBid(
        Object.freeze({
          ...prepared,
          bid: Object.freeze({ ...prepared?.bid, bidderCode: 'publisherBidder' }),
        })
      )
    ).toBeUndefined();

    disposers.reverse().forEach((dispose) => dispose());
    expect(release).toHaveBeenCalledOnce();
  });
});
