import { describe, expect, it, vi } from 'vitest';
import { JSDOM } from 'jsdom';

import {
  INITIAL_SLICE_DEFINITIONS,
  selectInitialSliceDefinitions,
} from '../../src/first_display/composition';
import { DIDOMI_INITIAL_SLICE } from '../../src/first_display/slices/didomi';
import { CREATIVE_INITIAL_SLICE } from '../../src/first_display/slices/creative';
import { APS_INITIAL_SLICE } from '../../src/first_display/slices/aps';
import { GPT_INITIAL_SLICE } from '../../src/first_display/slices/gpt';
import { PREBID_INITIAL_SLICE } from '../../src/first_display/slices/prebid';
import { DATADOME_INITIAL_SLICE } from '../../src/first_display/slices/datadome';
import { GOOGLE_TAG_MANAGER_INITIAL_SLICE } from '../../src/first_display/slices/google_tag_manager';
import { LOCKR_INITIAL_SLICE } from '../../src/first_display/slices/lockr';
import { OSANO_INITIAL_SLICE } from '../../src/first_display/slices/osano';
import { PERMUTIVE_INITIAL_SLICE } from '../../src/first_display/slices/permutive';
import { SOURCEPOINT_INITIAL_SLICE } from '../../src/first_display/slices/sourcepoint';
import { TESTLIGHT_INITIAL_SLICE } from '../../src/first_display/slices/testlight';
import type { FirstDisplayRouteRuleV1 } from '../../src/first_display/leaf/route_guard';
import { installDidomiInitial } from '../../src/first_display/leaf/config_guard';
import type { FirstDisplayCreativeGuardV1 } from '../../src/first_display/leaf/creative_guard';
import {
  installApsInitial,
  type FirstDisplayApsProtocolV1,
} from '../../src/first_display/leaf/aps_protocol';
import {
  installGptInitial,
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
import { shouldProxyExternalUrl } from '../../src/integrations/creative/proxy_sign';
import { getPermutiveSegments } from '../../src/integrations/permutive/segments';
import { mirrorSourcepointConsent } from '../../src/integrations/sourcepoint/consent_mirror';

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
    prepare: () => Object.freeze({ activate: () => undefined }),
  });
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
      installGptInitial(Object.freeze({ observe, register }), own, () =>
        Object.freeze({
          start: () => true,
          closeIngress: () => true,
          captureHandoff: () => Object.freeze([]),
          captureDiagnosticsHandoff: () =>
            Object.freeze({
              cycles: Object.freeze([]),
              facts: Object.freeze([]),
              nextTraceTokenOrdinal: 1,
              overflowCount: 0,
              dropCount: 0,
            }),
          detachCommittedSlots: () => true,
          dispose: () => undefined,
        })
      ),
      installPrebidInitial(Object.freeze({ observe, register }), own),
    ];

    expect(receipts).toEqual([
      { version: 1, id: 'aps' },
      { version: 1, id: 'gpt' },
      { version: 1, id: 'prebid' },
    ]);
    for (const receipt of receipts) {
      expect(Reflect.ownKeys(receipt)).toEqual(['version', 'id']);
      expect(Object.isFrozen(receipt)).toBe(true);
    }
    expect(register).toHaveBeenCalledTimes(3);
    for (const [protocol] of register.mock.calls) {
      expect(Reflect.ownKeys(protocol).length).toBeGreaterThan(2);
      expect(Object.isFrozen(protocol)).toBe(true);
    }
  });

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
      slice.activate(Object.freeze({ own: () => undefined, afterActivate: () => undefined }));
    expect(events).toEqual(INITIAL_SLICE_DEFINITIONS.map(({ id }) => id));
    expect(
      INITIAL_SLICE_DEFINITIONS.every(
        (definition) => Reflect.ownKeys(definition).join(',') === 'id,prepare'
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
          install?: (candidate: unknown, ownEffect: (release: () => void) => void) => void
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
            own
          ),
      });
      fixture.definition.prepare(host).activate(
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
        install?: (candidate: unknown, ownEffect: (release: () => void) => void) => void
      ) => install?.(bindings, own),
    });

    LOCKR_INITIAL_SLICE.prepare(host).activate(
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
    let contributor: (() => Readonly<Record<string, unknown>> | undefined) | undefined;
    let route: FirstDisplayContextRouteRuleV1 | undefined;
    const unregisterContext = vi.fn();
    const unregisterRoute = vi.fn();
    const bindings = Object.freeze({
      clearTimer: () => undefined,
      getSdk: () => sdk,
      host: 'publisher.example',
      observe: () => undefined,
      origin: 'https://publisher.example',
      protocol: 'https:',
      readStorage: (key: string) => localStorage.getItem(key),
      registerContext: (candidate: typeof contributor) => {
        contributor = candidate;
        return unregisterContext;
      },
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
        install?: (candidate: unknown, ownEffect: (release: () => void) => void) => void
      ) => {
        expect(id).toBe('permutive_initial');
        install?.(bindings, own);
      },
    });

    PERMUTIVE_INITIAL_SLICE.prepare(host).activate(
      Object.freeze({
        own: (release: () => void) => disposers.push(release),
        afterActivate: () => undefined,
      })
    );

    expect(contributor?.()).toEqual({
      permutive_segments: getPermutiveSegments(),
    });
    expect((contributor?.()?.permutive_segments as readonly string[]).length).toBe(100);
    expect(route?.matches('script', 'https://cdn.permutive.com/example-web.js')).toBe(true);
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
    expect(unregisterContext).toHaveBeenCalledOnce();
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
        install?: (candidate: unknown, ownEffect: (release: () => void) => void) => void
      ) => {
        expect(id).toBe('sourcepoint_initial');
        install?.(bindings, own);
      },
    });

    SOURCEPOINT_INITIAL_SLICE.prepare(host).activate(
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
        install?: (candidate: unknown, ownEffect: (release: () => void) => void) => void
      ) => {
        expect(id).toBe('osano_initial');
        install?.(bindings, own);
      },
    });

    OSANO_INITIAL_SLICE.prepare(host).activate(
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
    const unregisterContext = vi.fn();
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
        registerContext: () => unregisterContext,
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
    expect(unregisterContext).toHaveBeenCalledOnce();
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

  it('registers the exact creative parser guard policy and owns its rollback', () => {
    const release = vi.fn();
    const observations: Array<readonly [string, number]> = [];
    let guard: FirstDisplayCreativeGuardV1 | undefined;
    const disposers: Array<() => void> = [];
    const bindings = Object.freeze({
      config: Object.freeze({
        version: 1,
        enabled: true,
        clickGuard: true,
        renderGuard: true,
      }),
      location: Object.freeze({
        href: 'https://publisher.example/page',
        origin: 'https://publisher.example',
      }),
      observe: (name: string, value: number) => observations.push([name, value]),
      register: (candidate: FirstDisplayCreativeGuardV1) => {
        guard = candidate;
        return release;
      },
    });
    const host = Object.freeze({
      activate: (
        id: string,
        own: (dispose: () => void) => void,
        install?: (candidate: unknown, ownEffect: (dispose: () => void) => void) => void
      ) => {
        expect(id).toBe('creative_initial');
        install?.(bindings, own);
      },
    });

    CREATIVE_INITIAL_SLICE.prepare(host).activate(
      Object.freeze({
        own: (dispose: () => void) => disposers.push(dispose),
        afterActivate: () => undefined,
      })
    );
    expect(guard).toBeDefined();
    expect(guard).toMatchObject({
      clickGuard: true,
      id: 'creative',
      renderGuard: true,
      version: 1,
    });
    expect(Object.isFrozen(guard)).toBe(true);
    expect(guard?.normalizeNavigation('/landing')).toBe('https://publisher.example/landing');
    expect(guard?.normalizeNavigation('javascript:alert(1)')).toBeUndefined();
    expect(guard?.normalizeNavigation('https://user:pass@example.com/')).toBeUndefined();
    expect(guard?.shouldProxyResource('https://cdn.example/pixel.gif')).toBe(true);
    expect(guard?.shouldProxyResource('/publisher.png')).toBe(false);
    expect(guard?.shouldProxyResource('data:image/gif;base64,a')).toBe(false);
    for (const url of [
      'https://cdn.example/pixel.gif',
      'https://user:pass@cdn.example/pixel.gif',
      'data:image/gif;base64,a',
      'javascript:alert(1)',
      'blob:https://publisher.example/id',
      'not a valid URL',
    ]) {
      expect(guard?.shouldProxyResource(url)).toBe(shouldProxyExternalUrl(url));
    }
    expect(observations).toEqual([['guard_count', 3]]);

    disposers.reverse().forEach((dispose) => dispose());
    expect(release).toHaveBeenCalledOnce();
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
      const register = vi.fn();
      const host = Object.freeze({
        activate: (
          _id: string,
          own: (dispose: () => void) => void,
          install?: (candidate: unknown, ownEffect: (dispose: () => void) => void) => void
        ) =>
          install?.(
            Object.freeze({
              config,
              location: Object.freeze({
                href: 'https://publisher.example/page',
                origin: 'https://publisher.example',
              }),
              observe: () => undefined,
              register,
            }),
            own
          ),
      });
      expect(() =>
        CREATIVE_INITIAL_SLICE.prepare(host).activate(
          Object.freeze({ own: () => undefined, afterActivate: () => undefined })
        )
      ).toThrow();
      expect(register).not.toHaveBeenCalled();
    }
  });

  it('registers the closed APS reservation and document-channel protocol', () => {
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
        install?: (candidate: unknown, ownEffect: (dispose: () => void) => void) => void
      ) => {
        expect(id).toBe('aps_initial');
        install?.(bindings, own);
      },
    });

    APS_INITIAL_SLICE.prepare(host).activate(
      Object.freeze({
        own: (dispose: () => void) => disposers.push(dispose),
        afterActivate: () => undefined,
      })
    );
    expect(protocol?.rendererUrl).toBe('https://publisher.example/integrations/aps/renderer/v1');
    expect(protocol?.publisherOrigin).toBe('https://publisher.example');
    expect(protocol?.deadlines).toEqual({
      insertionMs: 1_000,
      documentAcceptanceMs: 3_000,
      completionMs: 10_000,
      ownerSettlementMs: 20_000,
    });
    expect(protocol?.isReservationId(`r1_${'a'.repeat(22)}`)).toBe(true);
    expect(protocol?.isLifecycleTicket(`t1_${'a'.repeat(22)}`)).toBe(true);
    expect(protocol?.isRendererNonce(`n1_${'a'.repeat(22)}`)).toBe(true);
    expect(protocol?.isReservationId(`r1_${'a'.repeat(21)}`)).toBe(false);
    const nonce = `n1_${'b'.repeat(22)}`;
    expect(
      protocol?.parseDocumentMessage(
        Object.freeze({ message: 'TS APS Document Accepted', version: 1, nonce }),
        nonce
      )
    ).toEqual({ kind: 'document_accepted' });
    expect(
      protocol?.parseDocumentMessage(
        Object.freeze({ message: 'TS APS Runner Loaded', version: 1, nonce }),
        nonce
      )
    ).toEqual({ kind: 'runner_loaded' });
    expect(
      protocol?.parseDocumentMessage(
        Object.freeze({ message: 'TS APS Render Completed', version: 1, nonce }),
        nonce
      )
    ).toEqual({ kind: 'render_completed' });
    expect(
      protocol?.parseDocumentMessage(
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
      expect(protocol?.parseDocumentMessage(Object.freeze(message), nonce)).toBeUndefined();
    }

    disposers.reverse().forEach((dispose) => dispose());
    expect(release).toHaveBeenCalledOnce();
  });

  it('registers the GPT request-cycle and targeting policy before any initial action', () => {
    const release = vi.fn();
    const disposers: Array<() => void> = [];
    let protocol: FirstDisplayGptProtocolV1 | undefined;
    const bindings = Object.freeze({
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
        install?: (candidate: unknown, ownEffect: (dispose: () => void) => void) => void
      ) => {
        expect(id).toBe('gpt_initial');
        install?.(bindings, own);
      },
    });

    GPT_INITIAL_SLICE.prepare(host).activate(
      Object.freeze({
        own: (dispose: () => void) => disposers.push(dispose),
        afterActivate: () => undefined,
      })
    );
    expect(protocol?.deadlines).toEqual({
      externalReadyMs: 10_000,
      requestStartMs: 3_000,
      completionMs: 10_000,
    });
    expect(protocol?.createBatch).toBeTypeOf('function');
    expect(
      protocol?.requestPlan(
        Object.freeze({ initialLoadDisabled: false, ownership: 'trusted_server' })
      )
    ).toEqual({ operations: ['display'], requestOperation: 0 });
    expect(
      protocol?.requestPlan(
        Object.freeze({ initialLoadDisabled: true, ownership: 'trusted_server' })
      )
    ).toEqual({ operations: ['display', 'refresh'], requestOperation: 1 });
    expect(
      protocol?.requestPlan(Object.freeze({ initialLoadDisabled: false, ownership: 'publisher' }))
    ).toEqual({ operations: ['refresh'], requestOperation: 0 });
    expect(protocol?.validTargetingValue('x'.repeat(40))).toBe(true);
    expect(protocol?.validTargetingValue('😀'.repeat(40))).toBe(true);
    expect(protocol?.validTargetingValue('x'.repeat(41))).toBe(false);
    expect(protocol?.validTargetingValue('😀'.repeat(41))).toBe(false);
    expect(protocol?.validTargetingValue('bad\nvalue')).toBe(false);
    expect(protocol?.classifyRenderEnded(Object.freeze({ isEmpty: true }))).toBe('gam_empty');
    expect(protocol?.classifyRenderEnded(Object.freeze({ isEmpty: false }))).toBe('nonempty_gam');
    expect(protocol?.classifyRenderEnded(Object.freeze({ isEmpty: 'false' }))).toBeUndefined();

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
        install?: (candidate: unknown, ownEffect: (dispose: () => void) => void) => void
      ) => {
        expect(id).toBe('prebid_initial');
        install?.(bindings, own);
      },
    });

    PREBID_INITIAL_SLICE.prepare(host).activate(
      Object.freeze({
        own: (dispose: () => void) => disposers.push(dispose),
        afterActivate: () => undefined,
      })
    );
    expect(protocol).toMatchObject({
      bidderCode: 'trustedServer',
      maxPendingOperations: 64,
      externalReadyMs: 10_000,
      admissionLeaseMs: 10_000,
      renderReservationMs: 15 * 60 * 1_000,
    });
    expect(protocol?.normalizeEidSource('  ID5-SYNC.COM ')).toBe('id5-sync.com');
    expect(protocol?.normalizeEidSource('   ')).toBeUndefined();
    const prepared = protocol?.snapshotTrustedBid(
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
      protocol?.snapshotTrustedBid(
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
