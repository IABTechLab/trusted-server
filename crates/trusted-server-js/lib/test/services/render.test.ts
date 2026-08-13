import { describe, expect, it, vi } from 'vitest';

import apsEnvelope from '../fixtures/aps-renderer-v1.json';
import { createBrowserMessagingAdapter, type MessagingAdapter } from '../../src/adapters/messaging';
import { prepareAdmIframe } from '../../src/core/render';
import { resizeCollapsedPucShell } from '../../src/core/puc_shell';
import { createTestNavigationIdentityIssuer } from '../../src/kernel/identity';
import { createRuntimeSession } from '../../src/kernel/sessions';
import type { RenderAttemptScope, WinnerContext } from '../../src/kernel/sessions';
import {
  APS_RENDERER_SANDBOX,
  APS_RENDERER_V1_PATH,
  renderDirectApsAttempt,
  resolveApsRendererV1Url,
} from '../../src/integrations/aps/render';
import {
  createCommittedArtifactStore,
  createRenderAttempt,
  createRendererNonceRegistry,
  createSlotOperation,
  renderDirectCacheAttempt,
  renderDirectAdmAttempt,
  resolveCacheAdmAttempt,
  type CacheAdmSource,
  type CommittedRenderArtifact,
  type DirectAdmIframeConstructor,
  type DirectAdmIframeHandle,
  type RenderAttempt,
  type RenderAttemptDiagnosticsObservation,
  type RenderAttemptSnapshot,
  type RenderAttemptState,
  type SlotOperation,
  type SlotOperationOptions,
} from '../../src/services/render';
import {
  createReservationService,
  type ReservationClaimResult,
  type ReservationRenderSource,
  type ReservationService,
} from '../../src/services/reservations';

const ATTEMPT_ONE = 'a1_0000000000000000000000';
const ATTEMPT_TWO = 'a1_0000000000000000000001';

function indexedAttemptId(index: number): string {
  return `a1_${index.toString().padStart(22, '0')}`;
}

function indexedRendererNonce(index: number): string {
  return `n1_${index.toString().padStart(22, '0')}`;
}

const ADM_SOURCE = Object.freeze({
  type: 'adm' as const,
  version: 1 as const,
  adm: '<div>fictional creative</div>',
  width: 300,
  height: 250,
});

const APS_SOURCE = Object.freeze({
  type: 'aps' as const,
  version: 1 as const,
  accountId: 'fictional-account',
  bidId: 'fictional-bid',
  tagType: 'iframe' as const,
  creativeUrl: 'https://creative.example/render',
  width: 300,
  height: 250,
  aaxResponse: 'e30=',
});

const DIRECT_APS_BID = apsEnvelope.seatbid[0]!.bid[0]!;
const DIRECT_APS_SOURCE = Object.freeze({
  type: 'aps' as const,
  version: 1 as const,
  accountId: 'fictional-account',
  bidId: DIRECT_APS_BID.id,
  creativeId: 'fictional-creative',
  tagType: DIRECT_APS_BID.ext.tagtype as 'iframe',
  creativeUrl: DIRECT_APS_BID.ext.creativeurl,
  width: DIRECT_APS_BID.w,
  height: DIRECT_APS_BID.h,
  aaxResponse: btoa(JSON.stringify(apsEnvelope)),
});

const WINNER_CONTEXT = Object.freeze({ selectedCpm: 1 });
const CACHE_ID = 'f47447a0-b759-4f2f-9887-af458b79b570';
const CACHE_POLICY = Object.freeze({
  version: 1 as const,
  baseUrl: 'https://cache.example:8443/pbc/v1/cache',
});
const CACHE_SOURCE = Object.freeze({
  type: 'cache' as const,
  version: 1 as const,
  cacheId: CACHE_ID,
  fetchUrl: `${CACHE_POLICY.baseUrl}?uuid=${CACHE_ID}`,
  width: 300,
  height: 250,
});

describe('collapsed PUC shell resize', () => {
  function collapsedShell(): {
    readonly frame: HTMLIFrameElement;
    readonly wrapper: HTMLDivElement;
  } {
    const wrapper = document.createElement('div');
    const frame = document.createElement('iframe');
    wrapper.style.width = '1px';
    wrapper.style.height = '1px';
    frame.setAttribute('width', '1');
    frame.setAttribute('height', '1');
    frame.style.width = '1px';
    frame.style.height = '1px';
    wrapper.appendChild(frame);
    document.body.appendChild(wrapper);
    return { frame, wrapper };
  }

  it('resizes only the exact connected source iframe and its collapsed immediate wrapper once', () => {
    const selected = collapsedShell();
    const sibling = collapsedShell();

    try {
      expect(
        resizeCollapsedPucShell({
          source: selected.frame.contentWindow!,
          width: 300,
          height: 250,
        })
      ).toBe(true);
      expect(selected.frame.style.width).toBe('300px');
      expect(selected.frame.style.height).toBe('250px');
      expect(selected.wrapper.style.width).toBe('300px');
      expect(selected.wrapper.style.height).toBe('250px');
      expect(sibling.frame.style.width).toBe('1px');
      expect(sibling.wrapper.style.width).toBe('1px');

      expect(
        resizeCollapsedPucShell({
          source: selected.frame.contentWindow!,
          width: 300,
          height: 250,
        })
      ).toBe(false);
    } finally {
      selected.wrapper.remove();
      sibling.wrapper.remove();
    }
  });

  it('rejects invalid dimensions and non-ordinary, expanded, detached, or replaced shells atomically', () => {
    const cases: Array<(shell: ReturnType<typeof collapsedShell>) => void> = [
      ({ wrapper }) => {
        wrapper.style.width = '2px';
      },
      ({ frame }) => {
        frame.style.position = 'fixed';
      },
      ({ wrapper }) => {
        wrapper.style.position = 'sticky';
      },
      ({ wrapper }) => {
        wrapper.setAttribute('data-anchor-status', 'displayed');
      },
      ({ frame }) => {
        frame.remove();
      },
    ];

    for (const mutate of cases) {
      const shell = collapsedShell();
      const source = shell.frame.contentWindow!;
      mutate(shell);
      try {
        expect(resizeCollapsedPucShell({ source, width: 300, height: 250 })).toBe(false);
        expect(shell.wrapper.style.height).toBe('1px');
        expect(shell.frame.style.height).toBe('1px');
      } finally {
        shell.wrapper.remove();
      }
    }

    const invalid = collapsedShell();
    try {
      expect(
        resizeCollapsedPucShell({
          source: invalid.frame.contentWindow!,
          width: Number.NaN,
          height: 250,
        })
      ).toBe(false);
      expect(invalid.frame.style.width).toBe('1px');
      expect(invalid.wrapper.style.width).toBe('1px');
    } finally {
      invalid.wrapper.remove();
    }
  });

  it('rejects a collapsed ordinary wrapper nested inside an anchor shell', () => {
    const shell = collapsedShell();
    const anchor = document.createElement('a');
    shell.wrapper.replaceWith(anchor);
    anchor.appendChild(shell.wrapper);

    try {
      expect(
        resizeCollapsedPucShell({
          source: shell.frame.contentWindow!,
          width: 300,
          height: 250,
        })
      ).toBe(false);
      expect(shell.frame.style.width).toBe('1px');
      expect(shell.wrapper.style.width).toBe('1px');
    } finally {
      anchor.remove();
    }
  });
});

function prepareRenderSource(candidate: unknown) {
  if (candidate === ADM_SOURCE) return ADM_SOURCE;
  if (candidate === CACHE_SOURCE) return CACHE_SOURCE;
  if (candidate === APS_SOURCE) return APS_SOURCE;
  if (candidate === DIRECT_APS_SOURCE) return DIRECT_APS_SOURCE;
  return undefined;
}

const RESERVATION_ID = 'r1_0000000000000000000000';
const attemptReservations = new WeakMap<RenderAttempt, ReservationService>();
const matrixClaims = new WeakMap<RenderAttempt, object>();

function reservations(): ReservationService {
  return createReservationService({ now: () => 0, prepareRenderSource });
}

type TestOwner = RenderAttemptScope & {
  admitClaimedContext(context: WinnerContext): void;
  disposeFromNavigation(): void;
};

function owner(
  id = ATTEMPT_ONE,
  slot = 'fictional-slot',
  navigationGeneration = Object.freeze({})
): TestOwner {
  let current = true;
  let disposed = false;
  let winnerContext: WinnerContext | undefined;
  const callbacks: Array<() => void> = [];
  const controller = new AbortController();
  const scope = {
    id,
    slot,
    generation: Object.freeze({}),
    navigationGeneration,
    interfaces: Object.freeze({}),
    get disposed() {
      return disposed;
    },
    get signal() {
      return controller.signal;
    },
    get winnerContext() {
      return winnerContext;
    },
    capture:
      <Arguments extends readonly unknown[]>(callback: (...arguments_: Arguments) => unknown) =>
      (...arguments_: Arguments): boolean => {
        if (!scope.isCurrent()) return false;
        callback(...arguments_);
        return true;
      },
    isCurrent: () => current && !disposed,
    prepareWinnerContext: (context: WinnerContext) => {
      if (!scope.isCurrent()) return undefined;
      const previous = winnerContext;
      if (previous !== undefined && previous !== context) return undefined;
      let committed = false;
      return Object.freeze({
        commit: () => {
          if (committed) return winnerContext === context;
          if (!scope.isCurrent() || winnerContext !== previous) return false;
          winnerContext = context;
          committed = true;
          return true;
        },
        rollback: () => {
          if (committed && previous === undefined && winnerContext === context) {
            winnerContext = undefined;
          }
          committed = false;
          return winnerContext === previous;
        },
      });
    },
    onDispose: (_kind: string, callback: () => void) => {
      callbacks.push(callback);
    },
    dispose: () => {
      if (disposed) return;
      disposed = true;
      controller.abort();
      for (let index = callbacks.length - 1; index >= 0; index -= 1) callbacks[index]?.();
    },
    disposeFromNavigation: () => {
      current = false;
      scope.dispose();
    },
    admitClaimedContext: (context: WinnerContext) => {
      winnerContext = context;
    },
  } satisfies TestOwner;
  return scope;
}

function artifact(
  render: Pick<RenderAttemptScope, 'id' | 'slot' | 'navigationGeneration'>,
  kind: CommittedRenderArtifact['kind'] = 'direct_iframe'
): CommittedRenderArtifact & { dispose: ReturnType<typeof vi.fn> } {
  return Object.freeze({
    kind,
    attemptId: render.id,
    slot: render.slot,
    navigationGeneration: render.navigationGeneration,
    dispose: vi.fn(),
  });
}

function attempt(
  scope = owner(),
  options: Partial<Parameters<typeof createRenderAttempt>[0]> = {}
): RenderAttempt {
  const reservationService = options.reservations ?? reservations();
  const result = createRenderAttempt({
    artifacts: options.artifacts ?? createCommittedArtifactStore(),
    owner: scope,
    prepareRenderSource: options.prepareRenderSource ?? prepareRenderSource,
    reservations: reservationService,
    ...(options.parentAttemptId === undefined ? {} : { parentAttemptId: options.parentAttemptId }),
    ...(options.publishDiagnostics === undefined
      ? {}
      : { publishDiagnostics: options.publishDiagnostics }),
    ...(options.scheduler === undefined ? {} : { scheduler: options.scheduler }),
  });
  expect(result).toMatchObject({ ok: true });
  if (!result.ok) throw new Error('should create an attempt');
  attemptReservations.set(result.value, reservationService);
  return result.value;
}

function rendererPort() {
  return Object.freeze({ close: vi.fn() });
}

function browserMessagePort() {
  const listeners = new Set<(event: unknown) => void>();
  const messageErrorListeners = new Set<(event: unknown) => void>();
  return {
    addEventListener: vi.fn((type: string, listener: (event: unknown) => void) => {
      (type === 'messageerror' ? messageErrorListeners : listeners).add(listener);
    }),
    close: vi.fn(),
    emit(data: unknown): void {
      for (const listener of listeners) listener({ data });
    },
    emitError(): void {
      for (const listener of messageErrorListeners) listener({});
    },
    postMessage: vi.fn(),
    removeEventListener: vi.fn((type: string, listener: (event: unknown) => void) => {
      (type === 'messageerror' ? messageErrorListeners : listeners).delete(listener);
    }),
    start: vi.fn(),
  };
}

describe('renderer nonce registry', () => {
  it('admits exactly 256 active bindings and refuses the 257th without drawing', () => {
    let draw = 0;
    const mintNonce = vi.fn(() =>
      Object.freeze({ ok: true as const, value: indexedRendererNonce(draw++) })
    );
    const registry = createRendererNonceRegistry({ mintNonce });

    for (let index = 0; index < 257; index += 1) {
      const render = attempt(owner(indexedAttemptId(index), `slot-${index}`));
      const issued = registry.issue({
        attempt: render,
        source: Object.freeze({ index }),
        port: rendererPort(),
      });
      if (index < 256) {
        expect(issued).toEqual({ ok: true, nonce: indexedRendererNonce(index) });
        expect(registry.snapshotForTest()).toMatchObject({
          bindings: index + 1,
          liveNonces: index + 1,
        });
      } else {
        expect(issued).toEqual({ ok: false, reason: 'capability_registry_full' });
      }
    }
    expect(mintNonce).toHaveBeenCalledTimes(256);
  });

  it('uses eight total collision draws and contains identity-source failure', () => {
    const nonce = indexedRendererNonce(7);
    const collisionMint = vi.fn(() => Object.freeze({ ok: true as const, value: nonce }));
    const registry = createRendererNonceRegistry({ mintNonce: collisionMint });
    const first = attempt(owner(indexedAttemptId(1), 'slot-1'));
    const second = attempt(owner(indexedAttemptId(2), 'slot-2'));
    expect(
      registry.issue({ attempt: first, source: Object.freeze({}), port: rendererPort() })
    ).toEqual({ ok: true, nonce });
    expect(
      registry.issue({ attempt: second, source: Object.freeze({}), port: rendererPort() })
    ).toEqual({ ok: false, reason: 'identity_generation_failed' });
    expect(collisionMint).toHaveBeenCalledTimes(9);

    const failedMint = vi.fn(() =>
      Object.freeze({ ok: false as const, reason: 'identity_generation_failed' as const })
    );
    const failedRegistry = createRendererNonceRegistry({ mintNonce: failedMint });
    expect(
      failedRegistry.issue({
        attempt: attempt(owner(indexedAttemptId(3), 'slot-3')),
        source: Object.freeze({}),
        port: rendererPort(),
      })
    ).toEqual({ ok: false, reason: 'identity_generation_failed' });
    expect(failedMint).toHaveBeenCalledOnce();
  });

  it.each([
    ['undefined', () => undefined],
    ['null', () => null],
    ['primitive', () => 1],
    [
      'accessor',
      () =>
        Object.freeze(
          Object.defineProperties(
            {},
            {
              ok: {
                enumerable: true,
                get: () => {
                  throw new Error('sensitive issuer result');
                },
              },
              value: { enumerable: true, value: indexedRendererNonce(1) },
            }
          )
        ),
    ],
    [
      'proxy',
      () =>
        new Proxy(Object.freeze({ ok: true, value: indexedRendererNonce(1) }), {
          ownKeys: () => {
            throw new Error('sensitive issuer proxy');
          },
        }),
    ],
    [
      'malformed success',
      () => Object.freeze({ ok: true, value: indexedRendererNonce(1), unexpected: true }),
    ],
    ['malformed failure', () => Object.freeze({ ok: false, reason: 'different_failure' })],
  ])('fails closed for a hostile %s issuer result', (_label, hostileResult) => {
    const registry = createRendererNonceRegistry({
      mintNonce: hostileResult as never,
    });
    let result: unknown;
    expect(() => {
      result = registry.issue({
        attempt: attempt(owner(indexedAttemptId(9), 'slot-9')),
        source: Object.freeze({}),
        port: rendererPort(),
      });
    }).not.toThrow();
    expect(result).toEqual({ ok: false, reason: 'identity_generation_failed' });
  });

  it('consumes once only for the exact nonce, source, port, attempt, and generation', () => {
    const nonce = indexedRendererNonce(1);
    const registry = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: nonce }),
    });
    const render = attempt(owner(indexedAttemptId(1), 'slot-1'));
    const other = attempt(owner(indexedAttemptId(2), 'slot-2'));
    const source = Object.freeze({});
    const port = rendererPort();
    expect(registry.issue({ attempt: render, source, port })).toEqual({ ok: true, nonce });

    expect(
      registry.consume({
        nonce: indexedRendererNonce(2),
        attempt: render,
        generation: render.generation,
        source,
        port,
      })
    ).toBe(false);
    expect(
      registry.consume({
        nonce,
        attempt: other,
        generation: render.generation,
        source,
        port,
      })
    ).toBe(false);
    expect(
      registry.consume({
        nonce,
        attempt: render,
        generation: Object.freeze({}),
        source,
        port,
      })
    ).toBe(false);
    expect(
      registry.consume({
        nonce,
        attempt: render,
        generation: render.generation,
        source: Object.freeze({}),
        port,
      })
    ).toBe(false);
    expect(
      registry.consume({
        nonce,
        attempt: render,
        generation: render.generation,
        source,
        port: rendererPort(),
      })
    ).toBe(false);
    const exact = { nonce, attempt: render, generation: render.generation, source, port };
    expect(registry.consume(exact)).toBe(true);
    expect(registry.consume(exact)).toBe(false);
    expect(registry.snapshotForTest()).toMatchObject({ bindings: 1, liveNonces: 0 });
    expect(port.close).not.toHaveBeenCalled();
  });

  it('issues before insertion and binds exactly one later renderer source before consumption', () => {
    const nonce = indexedRendererNonce(1);
    const registry = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: nonce }),
    });
    const render = attempt(owner(indexedAttemptId(1), 'slot-1'));
    const other = attempt(owner(indexedAttemptId(2), 'slot-2'));
    const port = rendererPort();
    const source = Object.freeze({ window: true });
    const wrongSource = Object.freeze({ window: false });
    expect(registry.issue({ attempt: render, port })).toEqual({ ok: true, nonce });
    const exact = Object.freeze({
      nonce,
      attempt: render,
      generation: render.generation,
      source,
      port,
    });

    expect(registry.consume(exact)).toBe(false);
    expect(registry.bindSource(Object.freeze({ ...exact, nonce: indexedRendererNonce(2) }))).toBe(
      false
    );
    expect(registry.bindSource(Object.freeze({ ...exact, attempt: other }))).toBe(false);
    expect(registry.bindSource(Object.freeze({ ...exact, generation: Object.freeze({}) }))).toBe(
      false
    );
    expect(registry.bindSource(Object.freeze({ ...exact, source: wrongSource }))).toBe(true);
    expect(registry.bindSource(exact)).toBe(false);
    expect(registry.consume(exact)).toBe(false);
    expect(registry.consume(Object.freeze({ ...exact, source: wrongSource }))).toBe(true);
    expect(registry.consume(Object.freeze({ ...exact, source: wrongSource }))).toBe(false);
  });

  it('cannot bind a deferred renderer source after attempt or registry disposal', () => {
    let draw = 0;
    const registry = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: indexedRendererNonce(draw++) }),
    });
    const settled = attempt(owner(indexedAttemptId(1), 'slot-1'));
    const settledPort = rendererPort();
    const settledIssue = registry.issue({ attempt: settled, port: settledPort });
    if (!settledIssue.ok) throw new Error('Expected deferred binding');
    expect(settled.fail('internal_error')).toBe(true);
    expect(
      registry.bindSource(
        Object.freeze({
          nonce: settledIssue.nonce,
          attempt: settled,
          generation: settled.generation,
          source: Object.freeze({}),
          port: settledPort,
        })
      )
    ).toBe(false);
    expect(settledPort.close).toHaveBeenCalledOnce();

    const disposed = attempt(owner(indexedAttemptId(2), 'slot-2'));
    const disposedPort = rendererPort();
    const disposedIssue = registry.issue({ attempt: disposed, port: disposedPort });
    if (!disposedIssue.ok) throw new Error('Expected deferred binding');
    registry.dispose();
    expect(
      registry.bindSource(
        Object.freeze({
          nonce: disposedIssue.nonce,
          attempt: disposed,
          generation: disposed.generation,
          source: Object.freeze({}),
          port: disposedPort,
        })
      )
    ).toBe(false);
    expect(disposedPort.close).toHaveBeenCalledOnce();
  });

  it('lets exactly one nested deferred source bind win before a hostile outer replay', () => {
    const nonce = indexedRendererNonce(1);
    const registry = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: nonce }),
    });
    const render = attempt();
    const port = rendererPort();
    const nestedSource = Object.freeze({ nested: true });
    const outerSource = Object.freeze({ outer: true });
    expect(registry.issue({ attempt: render, port })).toEqual({ ok: true, nonce });
    const nestedExpectation = Object.freeze({
      nonce,
      attempt: render,
      generation: render.generation,
      source: nestedSource,
      port,
    });
    const outerExpectation = Object.freeze({
      nonce,
      attempt: render,
      generation: render.generation,
      source: outerSource,
      port,
    });
    let nested: boolean | undefined;
    let reentered = false;
    const replay = new Proxy(outerExpectation, {
      ownKeys: (target) => {
        if (!reentered) {
          reentered = true;
          nested = registry.bindSource(nestedExpectation);
        }
        return Reflect.ownKeys(target);
      },
    });

    expect(registry.bindSource(replay)).toBe(false);
    expect(nested).toBe(true);
    expect(registry.consume(outerExpectation)).toBe(false);
    expect(registry.consume(nestedExpectation)).toBe(true);
  });

  it('rejects cross-attempt retained-port reuse without taking failed-issue ownership', () => {
    let draw = 0;
    const registry = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: indexedRendererNonce(draw++) }),
    });
    const port = rendererPort();
    const first = attempt(owner(indexedAttemptId(1), 'slot-1'));
    const second = attempt(owner(indexedAttemptId(2), 'slot-2'));
    expect(registry.issue({ attempt: first, source: Object.freeze({}), port })).toMatchObject({
      ok: true,
    });
    expect(registry.issue({ attempt: second, source: Object.freeze({}), port })).toEqual({
      ok: false,
      reason: 'invalid_attempt',
    });
    expect(port.close).not.toHaveBeenCalled();
    expect(second.fail('internal_error')).toBe(true);
    expect(port.close).not.toHaveBeenCalled();
    expect(first.fail('internal_error')).toBe(true);
    expect(port.close).toHaveBeenCalledOnce();
    expect(
      registry.issue({
        attempt: attempt(owner(indexedAttemptId(3), 'slot-3')),
        source: Object.freeze({}),
        port,
      })
    ).toEqual({ ok: false, reason: 'invalid_attempt' });
    registry.dispose();
    expect(port.close).toHaveBeenCalledOnce();
  });

  it('retires a transferred port before close can reenter issuance', () => {
    let draw = 0;
    const registry = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: indexedRendererNonce(draw++) }),
    });
    const first = attempt(owner(indexedAttemptId(1), 'slot-1'));
    const second = attempt(owner(indexedAttemptId(2), 'slot-2'));
    let nested: unknown;
    const port = Object.freeze({
      close: vi.fn(() => {
        nested = registry.issue({ attempt: second, source: Object.freeze({}), port });
      }),
    });
    expect(registry.issue({ attempt: first, source: Object.freeze({}), port })).toMatchObject({
      ok: true,
    });
    expect(first.fail('internal_error')).toBe(true);
    expect(nested).toEqual({ ok: false, reason: 'invalid_attempt' });
    expect(port.close).toHaveBeenCalledOnce();
  });

  it('makes branded settlement registration and revalidation intrinsic under prototype mutation', () => {
    const registry = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: indexedRendererNonce(1) }),
    });
    const render = attempt(owner(indexedAttemptId(1), 'slot-1'));
    let closes = 0;
    const port = Object.freeze({
      close: () => {
        closes += 1;
      },
    });
    const nativePush = Array.prototype.push;
    const nativeSlice = Array.prototype.slice;
    let poisonCalls = 0;
    const push = vi.spyOn(Array.prototype, 'push').mockImplementation(function (
      this: unknown[],
      ...values
    ) {
      poisonCalls += 1;
      Reflect.apply(nativePush, this, values);
      throw new Error('hostile observer registration');
    });
    let sliceCalls = 0;
    const slice = vi.spyOn(Array.prototype, 'slice').mockImplementation(function (
      this: unknown[],
      start?: number,
      end?: number
    ) {
      sliceCalls += 1;
      const result = Reflect.apply(nativeSlice, this, [start, end]);
      if (sliceCalls >= 4) throw new Error('hostile post-registration snapshot');
      return result;
    });
    let issued: unknown;
    try {
      issued = registry.issue({ attempt: render, source: Object.freeze({}), port });
    } finally {
      slice.mockRestore();
      push.mockRestore();
    }
    expect(poisonCalls).toBe(0);
    expect(sliceCalls).toBe(0);
    expect(issued).toEqual({ ok: true, nonce: indexedRendererNonce(1) });
    expect(closes).toBe(0);
    expect(render.fail('internal_error')).toBe(true);
    expect(closes).toBe(1);
  });

  it('drains terminal observers intrinsically before prototype splice can throw', () => {
    const registry = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: indexedRendererNonce(1) }),
    });
    const render = attempt(owner(indexedAttemptId(1), 'slot-1'));
    let closes = 0;
    const port = Object.freeze({
      close: () => {
        closes += 1;
      },
    });
    expect(registry.issue({ attempt: render, source: Object.freeze({}), port })).toMatchObject({
      ok: true,
    });
    const nativeSplice = Array.prototype.splice;
    let spliceCalls = 0;
    const splice = vi.spyOn(Array.prototype, 'splice').mockImplementation(function (
      this: unknown[],
      start: number,
      deleteCount?: number
    ) {
      spliceCalls += 1;
      Reflect.apply(nativeSplice, this, [start, deleteCount]);
      throw new Error('hostile terminal observer drain');
    });
    let iteratorCalls = 0;
    const iterator = Object.getOwnPropertyDescriptor(Array.prototype, Symbol.iterator);
    Object.defineProperty(Array.prototype, Symbol.iterator, {
      configurable: true,
      writable: true,
      value: () => {
        iteratorCalls += 1;
        throw new Error('hostile terminal observer iteration');
      },
    });
    let settled: boolean | undefined;
    let thrown: unknown;
    try {
      settled = render.fail('internal_error');
    } catch (error) {
      thrown = error;
    } finally {
      if (iterator) Object.defineProperty(Array.prototype, Symbol.iterator, iterator);
      splice.mockRestore();
    }
    expect(thrown).toBeUndefined();
    expect(settled).toBe(true);
    expect(spliceCalls).toBe(0);
    expect(iteratorCalls).toBe(0);
    expect(closes).toBe(1);
  });

  it('binds pending and live issuance to the exact issued attempt generation', () => {
    let draw = 0;
    const registry = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: indexedRendererNonce(draw++) }),
    });
    const sharedOwner = owner(indexedAttemptId(1), 'slot-1');
    const first = attempt(sharedOwner);
    const second = attempt(sharedOwner);
    const secondPort = rendererPort();
    expect(
      registry.issue({ attempt: first, source: Object.freeze({}), port: rendererPort() })
    ).toMatchObject({ ok: true });
    expect(
      registry.issue({ attempt: second, source: Object.freeze({}), port: secondPort })
    ).toEqual({ ok: false, reason: 'invalid_attempt' });
    expect(secondPort.close).not.toHaveBeenCalled();

    const nestedOwner = owner(indexedAttemptId(2), 'slot-2');
    const outer = attempt(nestedOwner);
    const inner = attempt(nestedOwner);
    const innerPort = rendererPort();
    let nested: unknown;
    let recurse = true;
    const reentrantRegistry = createRendererNonceRegistry({
      mintNonce: () => {
        if (recurse) {
          recurse = false;
          nested = reentrantRegistry.issue({
            attempt: inner,
            source: Object.freeze({}),
            port: innerPort,
          });
        }
        return Object.freeze({ ok: true as const, value: indexedRendererNonce(9) });
      },
    });
    expect(
      reentrantRegistry.issue({
        attempt: outer,
        source: Object.freeze({}),
        port: rendererPort(),
      })
    ).toMatchObject({ ok: true });
    expect(nested).toEqual({ ok: false, reason: 'invalid_attempt' });
    expect(innerPort.close).not.toHaveBeenCalled();
  });

  it('cannot publish after the issuer reentrantly disposes the registry', () => {
    const port = rendererPort();
    const registry = createRendererNonceRegistry({
      mintNonce: () => {
        registry.dispose();
        return Object.freeze({ ok: true as const, value: indexedRendererNonce(1) });
      },
    });
    const issued = registry.issue({
      attempt: attempt(owner(indexedAttemptId(1), 'slot-1')),
      source: Object.freeze({}),
      port,
    });
    expect(issued).toEqual({ ok: false, reason: 'invalid_attempt' });
    expect(issued).not.toHaveProperty('nonce');
    expect(port.close).not.toHaveBeenCalled();
    expect(registry.snapshotForTest()).toEqual({
      bindings: 0,
      disposed: true,
      liveNonces: 0,
    });
  });

  it('reserves attempt and capacity before invoking a reentrant issuer', () => {
    let draw = 0;
    let reenter: (() => void) | undefined;
    const registry = createRendererNonceRegistry({
      mintNonce: () => {
        const callback = reenter;
        reenter = undefined;
        callback?.();
        return Object.freeze({ ok: true as const, value: indexedRendererNonce(draw++) });
      },
    });

    for (let index = 0; index < 255; index += 1) {
      expect(
        registry.issue({
          attempt: attempt(owner(indexedAttemptId(index), `slot-${index}`)),
          source: Object.freeze({}),
          port: rendererPort(),
        })
      ).toMatchObject({ ok: true });
    }
    const outerAttempt = attempt(owner(indexedAttemptId(255), 'slot-255'));
    const innerAttempt = attempt(owner(indexedAttemptId(256), 'slot-256'));
    let nestedCapacity: unknown;
    reenter = () => {
      nestedCapacity = registry.issue({
        attempt: innerAttempt,
        source: Object.freeze({}),
        port: rendererPort(),
      });
    };
    expect(
      registry.issue({
        attempt: outerAttempt,
        source: Object.freeze({}),
        port: rendererPort(),
      })
    ).toMatchObject({ ok: true });
    expect(nestedCapacity).toEqual({ ok: false, reason: 'capability_registry_full' });
    expect(registry.snapshotForTest()).toMatchObject({ bindings: 256, liveNonces: 256 });

    const sameAttempt = attempt(owner(indexedAttemptId(999), 'slot-999'));
    const sameInput = {
      attempt: sameAttempt,
      source: Object.freeze({}),
      port: rendererPort(),
    };
    let nestedSameAttempt: unknown;
    let recurse = true;
    const sameAttemptRegistry = createRendererNonceRegistry({
      mintNonce: () => {
        if (recurse) {
          recurse = false;
          nestedSameAttempt = sameAttemptRegistry.issue(sameInput);
        }
        return Object.freeze({ ok: true as const, value: indexedRendererNonce(998) });
      },
    });
    expect(sameAttemptRegistry.issue(sameInput)).toMatchObject({ ok: true });
    expect(nestedSameAttempt).toEqual({ ok: false, reason: 'invalid_attempt' });
    expect(sameAttemptRegistry.snapshotForTest()).toMatchObject({ bindings: 1, liveNonces: 1 });
  });

  it('lets exactly one nested exact consume win before a hostile outer replay', () => {
    const nonce = indexedRendererNonce(1);
    const registry = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: nonce }),
    });
    const render = attempt(owner(indexedAttemptId(1), 'slot-1'));
    const source = Object.freeze({});
    const port = rendererPort();
    expect(registry.issue({ attempt: render, source, port })).toEqual({ ok: true, nonce });
    const exact = Object.freeze({
      nonce,
      attempt: render,
      generation: render.generation,
      source,
      port,
    });
    let nested: boolean | undefined;
    let reentered = false;
    const replay = new Proxy(exact, {
      ownKeys: (target) => {
        if (!reentered) {
          reentered = true;
          nested = registry.consume(exact);
        }
        return Reflect.ownKeys(target);
      },
    });

    expect(registry.consume(replay)).toBe(false);
    expect(nested).toBe(true);
    expect(registry.consume(exact)).toBe(false);
  });

  it('closes and removes attempt-owned bindings on settlement with no nonce history', () => {
    const nonce = indexedRendererNonce(1);
    const registry = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: nonce }),
    });
    const first = attempt(owner(indexedAttemptId(1), 'slot-1'));
    const firstPort = rendererPort();
    const firstSource = Object.freeze({});
    expect(registry.issue({ attempt: first, source: firstSource, port: firstPort })).toEqual({
      ok: true,
      nonce,
    });
    expect(
      registry.consume({
        nonce,
        attempt: first,
        generation: first.generation,
        source: firstSource,
        port: firstPort,
      })
    ).toBe(true);
    expect(
      registry.issue({ attempt: first, source: Object.freeze({}), port: rendererPort() })
    ).toEqual({ ok: false, reason: 'invalid_attempt' });
    expect(first.fail('internal_error')).toBe(true);
    expect(first.fail('internal_error')).toBe(false);
    expect(firstPort.close).toHaveBeenCalledOnce();
    expect(registry.snapshotForTest()).toEqual({
      bindings: 0,
      disposed: false,
      liveNonces: 0,
    });

    const second = attempt(owner(indexedAttemptId(2), 'slot-2'));
    expect(
      registry.issue({ attempt: second, source: Object.freeze({}), port: rendererPort() })
    ).toEqual({ ok: true, nonce });
    expect(
      registry.issue({ attempt: second, source: Object.freeze({}), port: rendererPort() })
    ).toEqual({ ok: false, reason: 'invalid_attempt' });
  });

  it('disposes live and consumed runtime bindings exactly once and remains terminal', () => {
    let draw = 0;
    const registry = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: indexedRendererNonce(draw++) }),
    });
    const live = attempt(owner(indexedAttemptId(1), 'slot-1'));
    const consumed = attempt(owner(indexedAttemptId(2), 'slot-2'));
    const liveSource = Object.freeze({ live: true });
    const consumedSource = Object.freeze({ consumed: true });
    let liveCloses = 0;
    let consumedCloses = 0;
    const livePort = Object.freeze({ close: () => (liveCloses += 1) });
    const consumedPort = Object.freeze({ close: () => (consumedCloses += 1) });
    const liveIssue = registry.issue({ attempt: live, source: liveSource, port: livePort });
    const consumedIssue = registry.issue({
      attempt: consumed,
      source: consumedSource,
      port: consumedPort,
    });
    if (!liveIssue.ok || !consumedIssue.ok) throw new Error('Expected nonce bindings');
    const consumedExpectation = Object.freeze({
      nonce: consumedIssue.nonce,
      attempt: consumed,
      generation: consumed.generation,
      source: consumedSource,
      port: consumedPort,
    });
    expect(registry.consume(consumedExpectation)).toBe(true);

    const iterator = Object.getOwnPropertyDescriptor(Array.prototype, Symbol.iterator);
    let iteratorCalls = 0;
    Object.defineProperty(Array.prototype, Symbol.iterator, {
      configurable: true,
      writable: true,
      value: () => {
        iteratorCalls += 1;
        throw new Error('hostile registry disposal iteration');
      },
    });
    let disposeError: unknown;
    try {
      registry.dispose();
    } catch (error) {
      disposeError = error;
    } finally {
      if (iterator) Object.defineProperty(Array.prototype, Symbol.iterator, iterator);
    }
    expect(disposeError).toBeUndefined();
    expect(iteratorCalls).toBe(0);
    expect(liveCloses).toBe(1);
    expect(consumedCloses).toBe(1);
    expect(registry.snapshotForTest()).toEqual({
      bindings: 0,
      disposed: true,
      liveNonces: 0,
    });
    registry.dispose();
    expect(live.fail('internal_error')).toBe(true);
    expect(consumed.fail('internal_error')).toBe(true);
    expect(liveCloses).toBe(1);
    expect(consumedCloses).toBe(1);
    expect(registry.consume(consumedExpectation)).toBe(false);

    const rejectedPort = rendererPort();
    expect(
      registry.issue({
        attempt: attempt(owner(indexedAttemptId(3), 'slot-3')),
        source: Object.freeze({}),
        port: rejectedPort,
      })
    ).toEqual({ ok: false, reason: 'invalid_attempt' });
    expect(rejectedPort.close).not.toHaveBeenCalled();
  });
});

describe('direct APS attempt rendering', () => {
  it('accepts no document-port traffic before the native load handoff', () => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const retainedRaw = browserMessagePort();
    const transferredRaw = browserMessagePort();
    const nonce = indexedRendererNonce(1);
    const messaging = createBrowserMessagingAdapter({
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const nonces = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: nonce }),
    });

    try {
      expect(
        renderDirectApsAttempt({
          attempt: render,
          container: document.getElementById('fictional-slot')!,
          messaging,
          nonces,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      expect(transferredRaw.postMessage).not.toHaveBeenCalled();
      retainedRaw.emit({ message: 'TS APS Document Accepted', version: 1, nonce });
      retainedRaw.emit({ message: 'TS APS Render Completed', version: 1, nonce });
      expect(render.snapshot()).toMatchObject({
        outcome: undefined,
        state: 'waiting_for_document',
      });

      const frame = document.querySelector<HTMLIFrameElement>('#fictional-slot iframe')!;
      frame.dispatchEvent(new Event('load'));
      retainedRaw.emit({ message: 'TS APS Document Accepted', version: 1, nonce });
      retainedRaw.emit({ message: 'TS APS Render Completed', version: 1, nonce });
      expect(render.snapshot().outcome).toEqual({ outcome: 'accepted' });
    } finally {
      nonces.dispose();
      document.body.innerHTML = '';
      vi.useRealTimers();
    }
  });

  it('uses captured native creation instead of a connected iframe returned by a hostile factory', () => {
    document.body.innerHTML =
      '<div id="publisher-owned"><iframe title="publisher frame"></iframe></div><div id="fictional-slot"></div>';
    const publisherContainer = document.getElementById('publisher-owned')!;
    const publisherFrame = publisherContainer.querySelector('iframe')!;
    const render = attempt();
    expect(render.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const createElement = vi
      .spyOn(document, 'createElement')
      .mockReturnValueOnce(publisherFrame as HTMLIFrameElement);
    const retainedRaw = browserMessagePort();
    const transferredRaw = browserMessagePort();
    const messaging = createBrowserMessagingAdapter({
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const nonces = createRendererNonceRegistry();

    try {
      expect(
        renderDirectApsAttempt({
          attempt: render,
          container: document.getElementById('fictional-slot')!,
          messaging,
          nonces,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      expect(createElement).not.toHaveBeenCalled();
      expect(publisherFrame.parentNode).toBe(publisherContainer);
      expect(publisherFrame.title).toBe('publisher frame');
      expect(document.querySelector('#fictional-slot iframe')).not.toBe(publisherFrame);
      expect(render.cancel('caller_aborted')).toBe(true);
      expect(publisherFrame.parentNode).toBe(publisherContainer);
    } finally {
      createElement.mockRestore();
      nonces.dispose();
      document.body.innerHTML = '';
    }
  });

  it('ignores a detached poisoned iframe and keeps native source/removal authority', () => {
    document.body.innerHTML =
      '<div id="unrelated-publisher-dom"></div><div id="fictional-slot"></div>';
    const unrelated = document.getElementById('unrelated-publisher-dom')!;
    const poisoned = document.createElement('iframe');
    poisoned.title = 'publisher detached frame';
    const forgedSource = Object.freeze({ postMessage: vi.fn() });
    Object.defineProperty(poisoned, 'contentWindow', {
      configurable: true,
      get: () => forgedSource,
    });
    Object.defineProperty(poisoned, 'src', {
      configurable: true,
      get: () => 'https://publisher.example/lie',
      set: vi.fn(),
    });
    poisoned.getAttribute = vi.fn(() => 'https://publisher.example/lie');
    poisoned.addEventListener = vi.fn(() => {
      throw new Error('publisher listener');
    });
    poisoned.remove = vi.fn(() => unrelated.remove());
    const createElement = vi.spyOn(document, 'createElement').mockReturnValueOnce(poisoned);
    const render = attempt();
    expect(render.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const retainedRaw = browserMessagePort();
    const transferredRaw = browserMessagePort();
    const messaging = createBrowserMessagingAdapter({
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const nonces = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: indexedRendererNonce(1) }),
    });

    try {
      expect(
        renderDirectApsAttempt({
          attempt: render,
          container: document.getElementById('fictional-slot')!,
          messaging,
          nonces,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      expect(createElement).not.toHaveBeenCalled();
      expect(poisoned.parentNode).toBeNull();
      expect(poisoned.title).toBe('publisher detached frame');
      const exactFrame = document.querySelector<HTMLIFrameElement>('#fictional-slot iframe')!;
      const exactSource = exactFrame.contentWindow!;
      const exactPost = vi.spyOn(exactSource, 'postMessage');
      exactFrame.dispatchEvent(new Event('load'));
      expect(exactPost).toHaveBeenCalledOnce();
      expect(forgedSource.postMessage).not.toHaveBeenCalled();
      expect(poisoned.remove).not.toHaveBeenCalled();
      expect(unrelated.isConnected).toBe(true);
      expect(render.cancel('caller_aborted')).toBe(true);
      expect(unrelated.isConnected).toBe(true);
    } finally {
      createElement.mockRestore();
      nonces.dispose();
      document.body.innerHTML = '';
    }
  });

  it('disposes detached setup resources when listener installation throws before staging', () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const retained = Object.freeze({
      close: vi.fn(),
      listen: vi.fn(() => {
        throw new Error('hostile retained listener');
      }),
      post: vi.fn(),
    });
    const transferred = Object.freeze({
      close: vi.fn(),
      listen: vi.fn(),
      post: vi.fn(),
    });
    const messaging = Object.freeze({
      createChannel: () => Object.freeze({ retained, transferred }),
      postWindow: vi.fn(),
      installCaptureListener: vi.fn(),
      parseProtocolMessage: vi.fn(),
      extractTransferredPorts: vi.fn(),
    }) as unknown as MessagingAdapter;
    const nonces = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: indexedRendererNonce(1) }),
    });

    expect(
      renderDirectApsAttempt({
        attempt: render,
        container: document.getElementById('fictional-slot')!,
        messaging,
        nonces,
        publisherOrigin: window.location.origin,
      })
    ).toBe(false);
    expect(render.snapshot().outcome).toEqual({
      outcome: 'failed',
      reason: 'renderer_document_no_load',
    });
    expect(retained.close).toHaveBeenCalledOnce();
    expect(transferred.close).toHaveBeenCalledOnce();
    expect(document.querySelector('iframe')).toBeNull();
    nonces.dispose();
    document.body.innerHTML = '';
  });

  it('does not insert after a pre-append cancellation returns through setup', () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const container = document.getElementById('fictional-slot')!;
    const render = attempt();
    expect(render.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const retained = Object.freeze({
      close: vi.fn(),
      listen: vi.fn(() => {
        render.cancel('caller_aborted');
        return () => undefined;
      }),
      post: vi.fn(),
    });
    const transferred = Object.freeze({
      close: vi.fn(),
      listen: vi.fn(),
      post: vi.fn(),
    });
    const messaging = Object.freeze({
      createChannel: () => Object.freeze({ retained, transferred }),
      postWindow: vi.fn(),
      installCaptureListener: vi.fn(),
      parseProtocolMessage: vi.fn(),
      extractTransferredPorts: vi.fn(),
    }) as unknown as MessagingAdapter;
    const nonces = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: indexedRendererNonce(1) }),
    });
    const observer = new MutationObserver(() => undefined);
    observer.observe(container, { childList: true });

    expect(
      renderDirectApsAttempt({
        attempt: render,
        container,
        messaging,
        nonces,
        publisherOrigin: window.location.origin,
      })
    ).toBe(false);
    expect(render.snapshot().outcome).toEqual({
      outcome: 'cancelled',
      reason: 'caller_aborted',
    });
    expect(observer.takeRecords()).toHaveLength(0);
    expect(container.children).toHaveLength(0);
    expect(retained.close).toHaveBeenCalledOnce();
    expect(transferred.close).toHaveBeenCalledOnce();
    observer.disconnect();
    nonces.dispose();
    document.body.innerHTML = '';
  });

  it('binds the inserted renderer window and accepts only exact document-port completion', () => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="fictional-slot"><span>placeholder</span></div>';
    const artifacts = createCommittedArtifactStore();
    const render = attempt(owner(), { artifacts });
    expect(render.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const retainedRaw = browserMessagePort();
    const transferredRaw = browserMessagePort();
    const messaging = createBrowserMessagingAdapter({
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const nonce = indexedRendererNonce(1);
    const nonces = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: nonce }),
    });
    const container = document.getElementById('fictional-slot')!;

    try {
      expect(
        renderDirectApsAttempt({
          attempt: render,
          container,
          messaging,
          nonces,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      const iframe = container.querySelector('iframe');
      expect(iframe).not.toBeNull();
      expect(iframe?.src).toBe(
        `${new URL(APS_RENDERER_V1_PATH, window.location.origin).href}#tsaps=${nonce}`
      );
      expect(iframe?.getAttribute('sandbox')).toBe(APS_RENDERER_SANDBOX);
      expect(iframe?.width).toBe(String(DIRECT_APS_SOURCE.width));
      expect(iframe?.height).toBe(String(DIRECT_APS_SOURCE.height));
      expect(iframe?.style.width).toBe(`${DIRECT_APS_SOURCE.width}px`);
      expect(iframe?.style.height).toBe(`${DIRECT_APS_SOURCE.height}px`);
      expect(render.snapshot().state).toBe('waiting_for_document');

      const target = iframe?.contentWindow;
      if (!iframe || !target) throw new Error('Expected renderer window');
      const postMessage = vi.spyOn(target, 'postMessage');
      iframe.dispatchEvent(new Event('load'));
      expect(postMessage).toHaveBeenCalledWith(
        {
          version: 1,
          nonce,
          publisherOrigin: window.location.origin,
          renderer: DIRECT_APS_SOURCE,
        },
        '*',
        [transferredRaw]
      );

      retainedRaw.emit({
        message: 'TS APS Document Accepted',
        version: 1,
        nonce: indexedRendererNonce(2),
      });
      expect(render.snapshot().state).toBe('waiting_for_document');
      retainedRaw.emit({ message: 'TS APS Document Accepted', version: 1, nonce });
      expect(render.snapshot().state).toBe('waiting_for_aps_completion');
      retainedRaw.emit({ message: 'TS APS Runner Loaded', version: 1, nonce });
      expect(render.snapshot().outcome).toBeUndefined();
      expect(container.querySelector('span')).not.toBeNull();
      retainedRaw.emit({ message: 'TS APS Render Completed', version: 1, nonce });
      expect(render.snapshot().outcome).toEqual({ outcome: 'accepted' });
      expect(container.querySelector('span')).toBeNull();
      retainedRaw.emit({
        message: 'TS APS Render Failed',
        version: 1,
        nonce,
        reason: 'runner_failed',
      });
      expect(render.snapshot().outcome).toEqual({ outcome: 'accepted' });
      expect(iframe.isConnected).toBe(true);
      expect(retainedRaw.close).toHaveBeenCalledOnce();
      expect(transferredRaw.close).not.toHaveBeenCalled();
    } finally {
      artifacts.dispose();
      nonces.dispose();
      document.body.innerHTML = '';
      vi.useRealTimers();
    }
  });

  it('maps document and APS completion deadlines through the attempt-owned timers', () => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="document-slot"></div><div id="runner-slot"></div>';
    const makeRender = (id: string, slot: string) => {
      const render = attempt(owner(id, slot));
      expect(render.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
      const retainedRaw = browserMessagePort();
      const transferredRaw = browserMessagePort();
      const messaging = createBrowserMessagingAdapter({
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        MessageChannel: class {
          readonly port1 = retainedRaw;
          readonly port2 = transferredRaw;
        },
      });
      return { messaging, render, retainedRaw };
    };
    const first = makeRender(indexedAttemptId(1), 'document-slot');
    const second = makeRender(indexedAttemptId(2), 'runner-slot');
    let draw = 1;
    const nonces = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: indexedRendererNonce(draw++) }),
    });

    try {
      expect(
        renderDirectApsAttempt({
          attempt: first.render,
          container: document.getElementById('document-slot')!,
          messaging: first.messaging,
          nonces,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      vi.advanceTimersByTime(3_000);
      expect(first.render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'renderer_document_no_load',
      });
      expect(document.querySelector('#document-slot iframe')).toBeNull();

      expect(
        renderDirectApsAttempt({
          attempt: second.render,
          container: document.getElementById('runner-slot')!,
          messaging: second.messaging,
          nonces,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      const runnerFrame = document.querySelector<HTMLIFrameElement>('#runner-slot iframe')!;
      runnerFrame.dispatchEvent(new Event('load'));
      second.retainedRaw.emit({
        message: 'TS APS Document Accepted',
        version: 1,
        nonce: indexedRendererNonce(2),
      });
      second.retainedRaw.emit({
        message: 'TS APS Runner Loaded',
        version: 1,
        nonce: indexedRendererNonce(2),
      });
      vi.advanceTimersByTime(10_000);
      expect(second.render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'runner_failed',
      });
      expect(runnerFrame.isConnected).toBe(false);
    } finally {
      nonces.dispose();
      document.body.innerHTML = '';
      vi.useRealTimers();
    }
  });

  it.each([
    ['descriptor_invalid', 'winner_not_renderable'],
    ['runner_no_load', 'runner_no_load'],
    ['runner_failed', 'runner_failed'],
  ] as const)('maps static renderer %s to %s', (rendererReason, attemptReason) => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const retainedRaw = browserMessagePort();
    const transferredRaw = browserMessagePort();
    const messaging = createBrowserMessagingAdapter({
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const nonce = indexedRendererNonce(1);
    const nonces = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: nonce }),
    });

    try {
      expect(
        renderDirectApsAttempt({
          attempt: render,
          container: document.getElementById('fictional-slot')!,
          messaging,
          nonces,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      document.querySelector<HTMLIFrameElement>('iframe')?.dispatchEvent(new Event('load'));
      retainedRaw.emit({ message: 'TS APS Document Accepted', version: 1, nonce });
      retainedRaw.emit({
        message: 'TS APS Render Failed',
        version: 1,
        nonce,
        reason: rendererReason,
      });
      expect(render.snapshot().outcome).toEqual({ outcome: 'failed', reason: attemptReason });
      expect(document.querySelector('iframe')).toBeNull();
      expect(retainedRaw.close).toHaveBeenCalledOnce();
    } finally {
      nonces.dispose();
      document.body.innerHTML = '';
      vi.useRealTimers();
    }
  });

  it('removes and retires the pending frame and channel when caller cancellation wins', () => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const retainedRaw = browserMessagePort();
    const transferredRaw = browserMessagePort();
    const messaging = createBrowserMessagingAdapter({
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const nonce = indexedRendererNonce(1);
    const nonces = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: nonce }),
    });
    try {
      expect(
        renderDirectApsAttempt({
          attempt: render,
          container: document.getElementById('fictional-slot')!,
          messaging,
          nonces,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      const frame = document.querySelector<HTMLIFrameElement>('iframe')!;
      expect(render.cancel('caller_aborted')).toBe(true);
      expect(frame.isConnected).toBe(false);
      expect(retainedRaw.close).toHaveBeenCalledOnce();
      expect(transferredRaw.close).toHaveBeenCalledOnce();
      frame.dispatchEvent(new Event('load'));
      retainedRaw.emit({ message: 'TS APS Document Accepted', version: 1, nonce });
      retainedRaw.emit({ message: 'TS APS Render Completed', version: 1, nonce });
      expect(render.snapshot().outcome).toEqual({
        outcome: 'cancelled',
        reason: 'caller_aborted',
      });
    } finally {
      nonces.dispose();
      document.body.innerHTML = '';
      vi.useRealTimers();
    }
  });

  it('cannot accept a renderer frame removed before its load handoff', () => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const retainedRaw = browserMessagePort();
    const transferredRaw = browserMessagePort();
    const messaging = createBrowserMessagingAdapter({
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const nonce = indexedRendererNonce(1);
    const nonces = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: nonce }),
    });
    try {
      expect(
        renderDirectApsAttempt({
          attempt: render,
          container: document.getElementById('fictional-slot')!,
          messaging,
          nonces,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      const frame = document.querySelector<HTMLIFrameElement>('iframe')!;
      frame.remove();
      frame.dispatchEvent(new Event('load'));
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'renderer_document_no_load',
      });
      retainedRaw.emit({ message: 'TS APS Document Accepted', version: 1, nonce });
      retainedRaw.emit({ message: 'TS APS Render Completed', version: 1, nonce });
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'renderer_document_no_load',
      });
      expect(transferredRaw.close).toHaveBeenCalledOnce();
    } finally {
      nonces.dispose();
      document.body.innerHTML = '';
      vi.useRealTimers();
    }
  });

  it('cannot accept a renderer whose container ancestor is removed before handoff', () => {
    vi.useFakeTimers();
    document.body.innerHTML =
      '<section id="publisher-region"><div id="fictional-slot"></div></section>';
    const render = attempt();
    expect(render.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const retainedRaw = browserMessagePort();
    const transferredRaw = browserMessagePort();
    const messaging = createBrowserMessagingAdapter({
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const nonce = indexedRendererNonce(1);
    const nonces = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: nonce }),
    });
    const container = document.getElementById('fictional-slot')!;

    try {
      expect(
        renderDirectApsAttempt({
          attempt: render,
          container,
          messaging,
          nonces,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      const frame = container.querySelector('iframe')!;
      const target = frame.contentWindow!;
      const postMessage = vi.spyOn(target, 'postMessage');
      document.getElementById('publisher-region')!.remove();
      expect(frame.parentNode).toBe(container);
      expect(frame.isConnected).toBe(false);
      frame.dispatchEvent(new Event('load'));
      expect(postMessage).not.toHaveBeenCalled();
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'renderer_document_no_load',
      });
      retainedRaw.emit({ message: 'TS APS Document Accepted', version: 1, nonce });
      retainedRaw.emit({ message: 'TS APS Render Completed', version: 1, nonce });
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'renderer_document_no_load',
      });
    } finally {
      nonces.dispose();
      document.body.innerHTML = '';
      vi.useRealTimers();
    }
  });

  it('rejects a same-node src navigation before handoff', () => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="navigation-slot"></div>';
    const nonces = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: indexedRendererNonce(1) }),
    });

    const navigationRender = attempt(owner(indexedAttemptId(1), 'navigation-slot'));
    expect(navigationRender.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const navigationRetained = browserMessagePort();
    const navigationTransferred = browserMessagePort();
    const navigationMessaging = createBrowserMessagingAdapter({
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      MessageChannel: class {
        readonly port1 = navigationRetained;
        readonly port2 = navigationTransferred;
      },
    });

    try {
      expect(
        renderDirectApsAttempt({
          attempt: navigationRender,
          container: document.getElementById('navigation-slot')!,
          messaging: navigationMessaging,
          nonces,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      const navigationFrame = document.querySelector<HTMLIFrameElement>('#navigation-slot iframe')!;
      const originalSource = navigationFrame.contentWindow!;
      const postMessage = vi.spyOn(originalSource, 'postMessage');
      navigationFrame.src = 'https://attacker.example/replacement';
      navigationFrame.dispatchEvent(new Event('load'));
      expect(postMessage).not.toHaveBeenCalled();
      expect(navigationRender.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'renderer_document_no_load',
      });
    } finally {
      nonces.dispose();
      document.body.innerHTML = '';
      vi.useRealTimers();
    }
  });

  it('does not remove DOM installed reentrantly by accepted-settlement observers', () => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="fictional-slot"><span>placeholder</span></div>';
    const render = attempt();
    expect(render.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const retainedRaw = browserMessagePort();
    const transferredRaw = browserMessagePort();
    const messaging = createBrowserMessagingAdapter({
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const nonce = indexedRendererNonce(1);
    const nonces = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: nonce }),
    });
    const container = document.getElementById('fictional-slot')!;
    expect(
      render.onSettled((outcome) => {
        if (outcome.outcome !== 'accepted') return;
        const successor = document.createElement('div');
        successor.id = 'reentrant-successor';
        container.appendChild(successor);
      })
    ).toBe(true);

    try {
      expect(
        renderDirectApsAttempt({
          attempt: render,
          container,
          messaging,
          nonces,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      container.querySelector('iframe')?.dispatchEvent(new Event('load'));
      retainedRaw.emit({ message: 'TS APS Document Accepted', version: 1, nonce });
      const duringRenderSuccessor = document.createElement('div');
      duringRenderSuccessor.id = 'during-render-successor';
      container.appendChild(duringRenderSuccessor);
      retainedRaw.emit({ message: 'TS APS Render Completed', version: 1, nonce });
      expect(render.snapshot().outcome).toEqual({ outcome: 'accepted' });
      expect(container.querySelector('span')).toBeNull();
      expect(container.querySelector('#during-render-successor')).not.toBeNull();
      expect(container.querySelector('#reentrant-successor')).not.toBeNull();
    } finally {
      nonces.dispose();
      document.body.innerHTML = '';
      vi.useRealTimers();
    }
  });

  it('anchors a synchronous document deadline after insertion and removes the exact frame', () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt(owner(), {
      scheduler: Object.freeze({
        clear: vi.fn(),
        set: (callback: () => void) => {
          callback();
          return Object.freeze({});
        },
      }),
    });
    expect(render.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const retainedRaw = browserMessagePort();
    const transferredRaw = browserMessagePort();
    const messaging = createBrowserMessagingAdapter({
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const nonces = createRendererNonceRegistry({
      mintNonce: () => Object.freeze({ ok: true as const, value: indexedRendererNonce(1) }),
    });
    const container = document.getElementById('fictional-slot')!;
    const observer = new MutationObserver(() => undefined);
    observer.observe(container, { childList: true });

    expect(
      renderDirectApsAttempt({
        attempt: render,
        container,
        messaging,
        nonces,
        publisherOrigin: window.location.origin,
      })
    ).toBe(false);
    expect(render.snapshot().outcome).toEqual({
      outcome: 'failed',
      reason: 'renderer_document_no_load',
    });
    const mutations = observer.takeRecords();
    expect(mutations.some((mutation) => mutation.addedNodes.length === 1)).toBe(true);
    expect(mutations.some((mutation) => mutation.removedNodes.length === 1)).toBe(true);
    observer.disconnect();
    expect(container.querySelector('iframe')).toBeNull();
    expect(retainedRaw.close).toHaveBeenCalledOnce();
    expect(transferredRaw.close).toHaveBeenCalledOnce();
    nonces.dispose();
    document.body.innerHTML = '';
  });

  it('contains a hostile nonce-issuer result and closes both unowned channel endpoints', () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const retainedRaw = browserMessagePort();
    const transferredRaw = browserMessagePort();
    const messaging = createBrowserMessagingAdapter({
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      MessageChannel: class {
        readonly port1 = retainedRaw;
        readonly port2 = transferredRaw;
      },
    });
    const realNonces = createRendererNonceRegistry();
    const nonces = Object.freeze({
      ...realNonces,
      issue: () =>
        Object.freeze(
          Object.defineProperty({}, 'ok', {
            enumerable: true,
            get: () => {
              throw new Error('hostile nonce result');
            },
          })
        ),
    }) as unknown as typeof realNonces;
    let result: boolean | undefined;
    let thrown: unknown;
    try {
      result = renderDirectApsAttempt({
        attempt: render,
        container: document.getElementById('fictional-slot')!,
        messaging,
        nonces,
        publisherOrigin: window.location.origin,
      });
    } catch (error) {
      thrown = error;
    }

    expect(thrown).toBeUndefined();
    expect(result).toBe(false);
    expect(render.snapshot().outcome).toEqual({
      outcome: 'failed',
      reason: 'identity_generation_failed',
    });
    expect(retainedRaw.close).toHaveBeenCalledOnce();
    expect(transferredRaw.close).toHaveBeenCalledOnce();
    expect(document.querySelector('iframe')).toBeNull();
    realNonces.dispose();
    document.body.innerHTML = '';
  });

  it('rejects an invalid APS descriptor before creating a channel or mutating the DOM', () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const channelConstructor = vi.fn();
    const messaging = createBrowserMessagingAdapter({
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      MessageChannel: channelConstructor as never,
    });
    const nonces = createRendererNonceRegistry();
    const container = document.getElementById('fictional-slot')!;

    expect(
      renderDirectApsAttempt({
        attempt: render,
        container,
        messaging,
        nonces,
        publisherOrigin: window.location.origin,
      })
    ).toBe(false);
    expect(channelConstructor).not.toHaveBeenCalled();
    expect(container.children).toHaveLength(0);
    expect(render.snapshot().outcome).toEqual({
      outcome: 'failed',
      reason: 'winner_not_renderable',
    });
    nonces.dispose();
    document.body.innerHTML = '';
  });

  it('rejects a publisher origin that is not the exact container document origin', () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    const channelConstructor = vi.fn();
    const messaging = createBrowserMessagingAdapter({
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      MessageChannel: channelConstructor as never,
    });
    const nonces = createRendererNonceRegistry();
    const container = document.getElementById('fictional-slot')!;

    expect(
      renderDirectApsAttempt({
        attempt: render,
        container,
        messaging,
        nonces,
        publisherOrigin: 'https://foreign-publisher.example',
      })
    ).toBe(false);
    expect(channelConstructor).not.toHaveBeenCalled();
    expect(container.children).toHaveLength(0);
    expect(render.snapshot().outcome).toEqual({
      outcome: 'failed',
      reason: 'winner_not_renderable',
    });
    nonces.dispose();
    document.body.innerHTML = '';
  });

  it('allows HTTPS and loopback HTTP renderer origins but rejects production HTTP', () => {
    expect(resolveApsRendererV1Url('https://publisher.example')).toBe(
      'https://publisher.example/integrations/aps/renderer/v1'
    );
    expect(resolveApsRendererV1Url('http://localhost:8080')).toBe(
      'http://localhost:8080/integrations/aps/renderer/v1'
    );
    expect(resolveApsRendererV1Url('http://127.0.0.1:8080')).toBe(
      'http://127.0.0.1:8080/integrations/aps/renderer/v1'
    );
    expect(resolveApsRendererV1Url('http://[::1]:8080')).toBe(
      'http://[::1]:8080/integrations/aps/renderer/v1'
    );
    expect(resolveApsRendererV1Url('http://publisher.example')).toBeUndefined();
  });
});

function claimed(
  render: RenderAttempt,
  scope: TestOwner,
  source: ReservationRenderSource
): Extract<ReservationClaimResult, { claimed: true }> {
  const service = attemptReservations.get(render);
  if (!service) throw new Error('should own a reservation service');
  const registered = service.registerRender({
    reservationId: RESERVATION_ID,
    slot: scope.slot,
    navigation: {
      generation: scope.navigationGeneration,
      isCurrent: scope.isCurrent,
      onDispose: scope.onDispose,
    },
    attemptId: scope.id,
    renderSource: source,
    winnerContext: WINNER_CONTEXT,
  });
  if (!registered.ok) throw new Error('should register a render reservation');
  const result = service.claim({
    reservationId: RESERVATION_ID,
    slot: scope.slot,
    navigationGeneration: scope.navigationGeneration,
    attempt: scope,
    pucSource: Object.freeze({}),
  });
  if (!result.recognized || !result.claimed) throw new Error('should claim a reservation');
  return result;
}

function corsResponse(body: BodyInit, status = 200): Response {
  const response = new Response(body, { status });
  Object.defineProperty(response, 'type', { configurable: true, value: 'cors' });
  return response;
}

function cacheResponse(body: Uint8Array) {
  let delivered = false;
  const cancel = vi.fn(async () => undefined);
  return {
    cancel,
    response: Object.freeze({
      body: Object.freeze({
        getReader: () =>
          Object.freeze({
            cancel,
            read: async () => {
              if (delivered) return { done: true as const, value: undefined };
              delivered = true;
              return { done: false as const, value: body };
            },
            releaseLock: vi.fn(),
          }),
      }),
      ok: true,
      type: 'cors' as const,
    }) as unknown as Response,
  };
}

async function insertedCacheFrame(
  container: HTMLElement,
  render?: RenderAttempt
): Promise<HTMLIFrameElement> {
  await vi.waitFor(() =>
    expect({
      frame: container.querySelector('iframe'),
      snapshot: render?.snapshot(),
    }).toMatchObject({
      frame: expect.any(HTMLIFrameElement),
    })
  );
  const frame = container.querySelector('iframe');
  if (!frame) throw new Error('should insert a cache ADM iframe');
  return frame;
}

describe('direct cache attempt rendering', () => {
  it('uses the exact bounded CORS request and renders validated OpenRTB ADM through the shared constructor', async () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const context = Object.freeze({ selectedCpm: 1.25 });
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, context)).toBe(true);
    const container = document.getElementById('fictional-slot')!;
    const fetchCache = vi.fn(async () =>
      corsResponse(
        JSON.stringify({
          adm: '<div data-price="${AUCTION_PRICE}" data-b64="${AUCTION_PRICE:B64}">cached</div>',
          w: 300,
          h: 250,
          price: 999,
          id: 'fictional-openrtb-bid',
          ext: { ignored: true },
        })
      )
    );

    expect(
      renderDirectCacheAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        container,
        fetcher: fetchCache,
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(true);
    expect(render.snapshot()).toMatchObject({ outcome: undefined, state: 'rendering_direct' });
    const frame = await insertedCacheFrame(container, render);

    expect(fetchCache).toHaveBeenCalledWith(CACHE_SOURCE.fetchUrl, {
      credentials: 'omit',
      method: 'GET',
      mode: 'cors',
      redirect: 'error',
      referrer: '',
      referrerPolicy: 'no-referrer',
      signal: expect.any(AbortSignal),
    });
    expect(frame.srcdoc).toContain('data-price="1.25"');
    expect(frame.srcdoc).toContain('${AUCTION_PRICE:B64}');
    expect(frame.srcdoc).not.toContain('999');
    frame.dispatchEvent(new Event('load'));
    expect(render.snapshot().outcome).toEqual({ outcome: 'accepted' });
    document.body.innerHTML = '';
  });

  it.each(['basic', 'default', undefined] as const)(
    'rejects every response with non-CORS type %s',
    async (responseType) => {
      document.body.innerHTML = '<div id="fictional-slot"></div>';
      const render = attempt();
      expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);
      const container = document.getElementById('fictional-slot')!;

      const basicResponse = new Response(JSON.stringify({ adm: '<div>cached</div>' }));
      Object.defineProperty(basicResponse, 'type', { configurable: true, value: responseType });
      expect(
        renderDirectCacheAttempt({
          attempt: render,
          cachePolicy: CACHE_POLICY,
          container,
          fetcher: async () => basicResponse,
          prepareIframe: prepareAdmIframe,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);

      await vi.waitFor(() =>
        expect(render.snapshot().outcome).toEqual({
          outcome: 'failed',
          reason: 'cache_network_error',
        })
      );
      expect(container.querySelector('iframe')).toBeNull();
      document.body.innerHTML = '';
    }
  );

  it('rejects a basic response even when the cache and publisher origins match', async () => {
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);
    const response = new Response(JSON.stringify({ adm: '<div>same origin</div>' }));
    Object.defineProperty(response, 'type', { configurable: true, value: 'basic' });
    const onResolved = vi.fn<(source: CacheAdmSource) => boolean>(() => true);

    expect(
      resolveCacheAdmAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        fetcher: async () => response,
        onResolved,
      })
    ).toBe(true);
    await vi.waitFor(() =>
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'cache_network_error',
      })
    );
    expect(onResolved).not.toHaveBeenCalled();
  });

  it('accepts a CORS response when the cache and publisher origins match', async () => {
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);
    const onResolved = vi.fn<(source: CacheAdmSource) => boolean>(() => true);

    expect(
      resolveCacheAdmAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        fetcher: async () => corsResponse(JSON.stringify({ adm: '<div>wrong type</div>' })),
        onResolved,
      })
    ).toBe(true);
    await vi.waitFor(() => expect(onResolved).toHaveBeenCalledOnce());
    expect(onResolved.mock.calls[0]?.[0]).toMatchObject({ adm: '<div>wrong type</div>' });
    expect(render.snapshot()).toMatchObject({ outcome: undefined, state: 'rendering_direct' });
    expect(render.cancel('caller_aborted')).toBe(true);
  });

  it('terminally rejects a foreign direct-cache container before fetching or mutating DOM', () => {
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);
    const foreignContainer = document.implementation.createHTMLDocument().createElement('div');
    foreignContainer.append(document.createTextNode('foreign placeholder'));
    const fetcher = vi.fn();

    expect(
      renderDirectCacheAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        container: foreignContainer,
        fetcher,
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(false);
    expect(fetcher).not.toHaveBeenCalled();
    expect(foreignContainer.querySelector('iframe')).toBeNull();
    expect(foreignContainer.textContent).toBe('foreign placeholder');
    expect(render.snapshot().outcome).toEqual({
      outcome: 'failed',
      reason: 'winner_not_renderable',
    });
  });

  it('clears the fetch deadline at the final byte before preparing the ADM frame', async () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const clear = vi.fn();
    const render = attempt(owner(), {
      scheduler: Object.freeze({
        clear,
        set: vi.fn(() => Object.freeze({})),
      }),
    });
    expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);
    const container = document.getElementById('fictional-slot')!;
    const prepareIframe: DirectAdmIframeConstructor = (options) => {
      expect(clear).toHaveBeenCalledTimes(1);
      return prepareAdmIframe(options);
    };

    expect(
      renderDirectCacheAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        container,
        fetcher: async () => corsResponse(JSON.stringify({ adm: '<div>cached</div>' })),
        prepareIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(true);

    const frame = await insertedCacheFrame(container, render);
    frame.dispatchEvent(new Event('load'));
    expect(render.snapshot().outcome).toEqual({ outcome: 'accepted' });
    document.body.innerHTML = '';
  });

  it('keeps the admitted direct-cache winner context across delayed fetch and later winner changes', async () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, Object.freeze({ selectedCpm: 2.5 }))).toBe(true);
    const container = document.getElementById('fictional-slot')!;
    let resolveFetch: ((response: Response) => void) | undefined;
    const fetchCache = vi.fn(
      () =>
        new Promise<Response>((resolve) => {
          resolveFetch = resolve;
        })
    );

    expect(
      renderDirectCacheAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        container,
        fetcher: fetchCache,
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(true);
    const later = attempt(owner(ATTEMPT_TWO, 'later-slot'));
    expect(later.admitDirectWinner(CACHE_SOURCE, Object.freeze({ selectedCpm: 8.75 }))).toBe(true);
    resolveFetch?.(
      corsResponse(JSON.stringify({ adm: '<div>${AUCTION_PRICE}</div>', price: 1000 }))
    );
    const frame = await insertedCacheFrame(container);

    expect(frame.srcdoc).toContain('<div>2.5</div>');
    expect(frame.srcdoc).not.toContain('8.75');
    expect(frame.srcdoc).not.toContain('1000');
    frame.dispatchEvent(new Event('load'));
    expect(render.snapshot().outcome).toEqual({ outcome: 'accepted' });
    document.body.innerHTML = '';
  });

  it('does not let the generic direct transition bypass the cache-specific deadline', () => {
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);

    expect(render.beginDirect()).toBe(false);
    expect(render.snapshot()).toMatchObject({ outcome: undefined, state: 'created' });
    expect(render.cancel('caller_aborted')).toBe(true);
  });

  it.each([
    [
      'network rejection',
      () => Promise.reject(new TypeError('fictional CORS failure')),
      'cache_network_error',
    ],
    ['HTTP status', () => Promise.resolve(corsResponse('{}', 503)), 'cache_http_error'],
  ] as const)('maps %s to the exact typed cache failure', async (_case, fetchResult, reason) => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);

    expect(
      renderDirectCacheAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        container: document.getElementById('fictional-slot')!,
        fetcher: vi.fn(fetchResult),
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(true);
    await vi.waitFor(() =>
      expect(render.snapshot().outcome).toEqual({ outcome: 'failed', reason })
    );
    expect(render.snapshot().outcome?.outcome).not.toBe('no_bid');
    document.body.innerHTML = '';
  });

  it.each([
    ['opaque response', Object.freeze({ body: null, ok: true, type: 'opaque' })],
    [
      'throwing type accessor',
      Object.defineProperties(Object.create(null), {
        body: { enumerable: true, value: null },
        ok: { enumerable: true, value: true },
        type: {
          enumerable: true,
          get: () => {
            throw new Error('hostile response type');
          },
        },
      }),
    ],
    [
      'rejecting body reader',
      Object.freeze({
        body: Object.freeze({
          getReader: () =>
            Object.freeze({
              cancel: vi.fn(),
              read: async () => {
                throw new Error('fictional stream failure');
              },
              releaseLock: vi.fn(),
            }),
        }),
        ok: true,
        type: 'cors',
      }),
    ],
  ] as const)('contains a %s as cache_network_error', async (_case, response) => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);

    expect(
      renderDirectCacheAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        container: document.getElementById('fictional-slot')!,
        fetcher: async () => response,
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(true);
    await vi.waitFor(() =>
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'cache_network_error',
      })
    );
    expect(document.querySelector('iframe')).toBeNull();
    document.body.innerHTML = '';
  });

  it('resolves a delayed owner-controlled cache claim without constructing publisher DOM', async () => {
    document.body.innerHTML = '<div id="fictional-slot"><span>placeholder</span></div>';
    const scope = owner();
    const artifacts = createCommittedArtifactStore();
    const render = attempt(scope, { artifacts });
    expect(render.beginGamClaim()).toBe(true);
    expect(render.admitClaimedWinner(claimed(render, scope, CACHE_SOURCE))).toBe(true);
    expect(render.ownerClaimed()).toBe(true);
    expect(render.ownerRegistered()).toBe(true);
    let resolveFetch: ((response: Response) => void) | undefined;
    const fetchCache = vi.fn(
      () =>
        new Promise<Response>((resolve) => {
          resolveFetch = resolve;
        })
    );
    const container = document.getElementById('fictional-slot')!;
    const onResolved = vi.fn((_source: unknown) => true);

    expect(
      resolveCacheAdmAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        fetcher: fetchCache,
        onResolved,
      })
    ).toBe(true);
    expect(render.snapshot()).toMatchObject({
      outcome: undefined,
      state: 'waiting_for_insertion',
    });
    expect(fetchCache).toHaveBeenCalledOnce();
    expect(resolveFetch).toBeTypeOf('function');
    resolveFetch?.(
      corsResponse(JSON.stringify({ adm: '<div>${AUCTION_PRICE}</div>', price: 9000 }))
    );
    await vi.waitFor(() => expect(onResolved).toHaveBeenCalledOnce());
    const resolved = onResolved.mock.calls[0]?.[0];
    expect(resolved).toEqual({
      adm: '<div>1</div>',
      height: 250,
      type: 'adm',
      version: 1,
      width: 300,
    });
    expect(Object.isFrozen(resolved)).toBe(true);
    expect(render.snapshot()).toMatchObject({
      outcome: undefined,
      state: 'waiting_for_insertion',
    });
    expect(artifacts.current('fictional-slot')).toBeUndefined();
    expect(container.querySelector('iframe')).toBeNull();
    expect(container.querySelector('span')).not.toBeNull();
    expect(render.cancel('caller_aborted')).toBe(true);
    artifacts.dispose();
    document.body.innerHTML = '';
  });

  it('replaces the PUC cache deadline with the one-second owner-insertion deadline', async () => {
    vi.useFakeTimers();
    const scope = owner();
    const render = attempt(scope);
    expect(render.beginGamClaim()).toBe(true);
    expect(render.admitClaimedWinner(claimed(render, scope, CACHE_SOURCE))).toBe(true);
    expect(render.ownerClaimed()).toBe(true);
    expect(render.ownerRegistered()).toBe(true);
    let resolveFetch: ((response: Response) => void) | undefined;
    const fetcher = vi.fn(
      () =>
        new Promise<Response>((resolve) => {
          resolveFetch = resolve;
        })
    );
    const onResolved = vi.fn<(source: CacheAdmSource) => boolean>(() => true);

    try {
      expect(
        resolveCacheAdmAttempt({
          attempt: render,
          cachePolicy: CACHE_POLICY,
          fetcher,
          onResolved,
        })
      ).toBe(true);
      await vi.advanceTimersByTimeAsync(4_999);
      expect(render.snapshot().outcome).toBeUndefined();
      resolveFetch?.(corsResponse(JSON.stringify({ adm: '<div>cached</div>' })));
      for (let index = 0; index < 20 && onResolved.mock.calls.length === 0; index += 1) {
        await Promise.resolve();
      }
      expect(onResolved).toHaveBeenCalledOnce();
      expect(render.snapshot()).toMatchObject({
        outcome: undefined,
        state: 'waiting_for_insertion',
      });

      await vi.advanceTimersByTimeAsync(999);
      expect(render.snapshot().outcome).toBeUndefined();
      await vi.advanceTimersByTimeAsync(1);
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'owner_insertion_timeout',
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it('resolves a promoted Prebid cache lease from its captured projection after replacement', async () => {
    const scope = owner();
    const service = reservations();
    const render = attempt(scope, { reservations: service });
    const prebidBid = Object.freeze({ cpm: 3.25 });
    const navigation = Object.freeze({
      generation: scope.navigationGeneration,
      isCurrent: scope.isCurrent,
      onDispose: scope.onDispose,
    });
    let currentProjection: Readonly<{
      renderSource: ReservationRenderSource;
      winnerContext: WinnerContext;
    }> = Object.freeze({
      renderSource: CACHE_SOURCE,
      winnerContext: Object.freeze({ selectedCpm: 3.25 }),
    });
    expect(
      service.registerPrebidLease({
        reservationId: RESERVATION_ID,
        slot: scope.slot,
        navigation,
        auctionId: 'initial-auction',
        adUnitCode: scope.slot,
        renderSource: currentProjection.renderSource,
        winnerContext: currentProjection.winnerContext,
        prebidBid,
      })
    ).toMatchObject({ ok: true });

    currentProjection = Object.freeze({
      renderSource: Object.freeze({
        ...CACHE_SOURCE,
        cacheId: '00000000-0000-4000-8000-000000000001',
      }),
      winnerContext: Object.freeze({ selectedCpm: 99 }),
    });
    expect(currentProjection.winnerContext.selectedCpm).toBe(99);
    expect(render.beginGamClaim()).toBe(true);
    expect(
      service.promotePrebidSelection({
        reservationId: RESERVATION_ID,
        auctionId: 'initial-auction',
        adUnitCode: scope.slot,
        navigationGeneration: scope.navigationGeneration,
        attempt: scope,
        prebidBid,
      })
    ).toMatchObject({ ok: true });
    const claim = service.claim({
      reservationId: RESERVATION_ID,
      slot: scope.slot,
      navigationGeneration: scope.navigationGeneration,
      attempt: scope,
      pucSource: Object.freeze({ owner: 'puc' }),
    });
    expect(claim).toMatchObject({ recognized: true, claimed: true });
    expect(render.admitClaimedWinner(claim)).toBe(true);
    expect(render.ownerClaimed()).toBe(true);
    expect(render.ownerRegistered()).toBe(true);

    const fetcher = vi.fn(async (_input: string, _init: RequestInit) =>
      corsResponse(JSON.stringify({ adm: '<div>${AUCTION_PRICE}</div>', price: 99 }))
    );
    const onResolved = vi.fn<(source: CacheAdmSource) => boolean>(() => true);
    expect(
      resolveCacheAdmAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        fetcher,
        onResolved,
      })
    ).toBe(true);
    await vi.waitFor(() => expect(onResolved).toHaveBeenCalledOnce());

    expect(fetcher.mock.calls[0]?.[0]).toBe(CACHE_SOURCE.fetchUrl);
    expect(onResolved.mock.calls[0]?.[0]).toMatchObject({ adm: '<div>3.25</div>' });
    expect(onResolved.mock.calls[0]?.[0]).not.toMatchObject({ adm: '<div>99</div>' });
    expect(render.snapshot()).toMatchObject({ outcome: undefined, state: 'waiting_for_insertion' });
    expect(render.cancel('caller_aborted')).toBe(true);
  });

  it('keeps cache deadline completion private to the resolver capability', () => {
    const render = attempt();
    expect('beginCacheFetch' in render).toBe(false);
    expect('cacheFetchCompleted' in render).toBe(false);
  });

  it('classifies malformed UTF-8 as an invalid cache response', async () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);
    const malformed = cacheResponse(new Uint8Array([0xc3, 0x28]));

    expect(
      renderDirectCacheAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        container: document.getElementById('fictional-slot')!,
        fetcher: async () => malformed.response,
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(true);
    await vi.waitFor(() =>
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'cache_invalid_response',
      })
    );
    expect(document.querySelector('iframe')).toBeNull();
    document.body.innerHTML = '';
  });

  it.each([
    ['raw markup', '<div>raw</div>'],
    ['array', JSON.stringify([{ adm: '<div>wrapped</div>' }])],
    ['primitive', JSON.stringify('creative')],
    ['wrapper', JSON.stringify({ bid: { adm: '<div>wrapped</div>' } })],
    ['empty adm', JSON.stringify({ adm: '' })],
    ['width alias', JSON.stringify({ adm: '<div>alias</div>', width: 300, height: 250 })],
    ['unpaired w', JSON.stringify({ adm: '<div>unpaired</div>', w: 300 })],
    ['fractional dimensions', JSON.stringify({ adm: '<div>fractional</div>', w: 300.5, h: 250 })],
    ['out-of-range dimensions', JSON.stringify({ adm: '<div>large</div>', w: 4097, h: 250 })],
    ['mismatched dimensions', JSON.stringify({ adm: '<div>wrong</div>', w: 728, h: 90 })],
    ['negative price', JSON.stringify({ adm: '<div>price</div>', price: -1 })],
  ] as const)('rejects a cache %s response shape', async (_case, body) => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);

    expect(
      renderDirectCacheAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        container: document.getElementById('fictional-slot')!,
        fetcher: vi.fn(async () => corsResponse(body)),
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(true);
    await vi.waitFor(() =>
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'cache_invalid_response',
      })
    );
    expect(document.querySelector('iframe')).toBeNull();
    document.body.innerHTML = '';
  });

  it('keeps captured cache authorities when mutable globals are poisoned after module load', async () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);
    const response = corsResponse(JSON.stringify({ adm: 7 }));
    const nativeGetOwnPropertyDescriptor = Object.getOwnPropertyDescriptor;
    const descriptorDescriptor = Object.getOwnPropertyDescriptor(
      Object,
      'getOwnPropertyDescriptor'
    );
    const hasOwnDescriptor = Object.getOwnPropertyDescriptor(Object.prototype, 'hasOwnProperty');
    const finiteDescriptor = Object.getOwnPropertyDescriptor(Number, 'isFinite');
    const integerDescriptor = Object.getOwnPropertyDescriptor(Number, 'isInteger');
    const urlDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'URL');
    const encoderDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'TextEncoder');
    const decoderDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'TextDecoder');
    const abortDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'AbortController');

    try {
      Object.defineProperty(Object, 'getOwnPropertyDescriptor', {
        configurable: true,
        value: (target: object, name: PropertyKey) => {
          const descriptor = Reflect.apply(nativeGetOwnPropertyDescriptor, Object, [target, name]);
          return name === 'adm' && descriptor && 'value' in descriptor && descriptor.value === 7
            ? { ...descriptor, value: '<div>forged</div>' }
            : descriptor;
        },
        writable: true,
      });
      Object.defineProperty(Object.prototype, 'hasOwnProperty', {
        configurable: true,
        value: () => false,
        writable: true,
      });
      Object.defineProperty(Number, 'isFinite', {
        configurable: true,
        value: () => true,
        writable: true,
      });
      Object.defineProperty(Number, 'isInteger', {
        configurable: true,
        value: () => true,
        writable: true,
      });
      for (const name of ['URL', 'TextEncoder', 'TextDecoder', 'AbortController'] as const) {
        Object.defineProperty(globalThis, name, {
          configurable: true,
          value: class PoisonedAuthority {
            constructor() {
              throw new Error(`poisoned ${name}`);
            }
          },
          writable: true,
        });
      }

      expect(
        renderDirectCacheAttempt({
          attempt: render,
          cachePolicy: CACHE_POLICY,
          container: document.getElementById('fictional-slot')!,
          fetcher: async () => response,
          prepareIframe: prepareAdmIframe,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      for (let index = 0; index < 20 && render.snapshot().outcome === undefined; index += 1) {
        await Promise.resolve();
      }
    } finally {
      if (descriptorDescriptor) {
        Object.defineProperty(Object, 'getOwnPropertyDescriptor', descriptorDescriptor);
      }
      if (hasOwnDescriptor) {
        Object.defineProperty(Object.prototype, 'hasOwnProperty', hasOwnDescriptor);
      }
      if (finiteDescriptor) Object.defineProperty(Number, 'isFinite', finiteDescriptor);
      if (integerDescriptor) Object.defineProperty(Number, 'isInteger', integerDescriptor);
      if (urlDescriptor) Object.defineProperty(globalThis, 'URL', urlDescriptor);
      if (encoderDescriptor) Object.defineProperty(globalThis, 'TextEncoder', encoderDescriptor);
      if (decoderDescriptor) Object.defineProperty(globalThis, 'TextDecoder', decoderDescriptor);
      if (abortDescriptor) {
        Object.defineProperty(globalThis, 'AbortController', abortDescriptor);
      }
    }

    expect(render.snapshot().outcome).toEqual({
      outcome: 'failed',
      reason: 'cache_invalid_response',
    });
    expect(document.querySelector('iframe')).toBeNull();
    document.body.innerHTML = '';
  });

  it('enforces the 512 KiB streamed-body limit before JSON parsing', async () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);
    const oversized = new Uint8Array(512 * 1024 + 1);
    oversized.fill(0x20);

    expect(
      renderDirectCacheAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        container: document.getElementById('fictional-slot')!,
        fetcher: vi.fn(async () => corsResponse(oversized)),
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(true);
    await vi.waitFor(() =>
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'cache_invalid_response',
      })
    );
    document.body.innerHTML = '';
  });

  it('accepts an exact 512 KiB JSON body and bounded ADM', async () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);
    const prefix = '{"adm":"';
    const suffix = '"}';
    const exactBody = `${prefix}${'x'.repeat(512 * 1024 - prefix.length - suffix.length)}${suffix}`;
    expect(new TextEncoder().encode(exactBody)).toHaveLength(512 * 1024);

    expect(
      renderDirectCacheAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        container: document.getElementById('fictional-slot')!,
        fetcher: async () => corsResponse(exactBody),
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(true);
    const frame = await insertedCacheFrame(document.getElementById('fictional-slot')!, render);
    frame.dispatchEvent(new Event('load'));
    expect(render.snapshot().outcome).toEqual({ outcome: 'accepted' });
    document.body.innerHTML = '';
  });

  it('rejects an ADM whose auction-price expansion exceeds 512 KiB', async () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(
      render.admitDirectWinner(CACHE_SOURCE, Object.freeze({ selectedCpm: Number.MAX_VALUE }))
    ).toBe(true);
    const body = JSON.stringify({ adm: '${AUCTION_PRICE}'.repeat(25_000) });
    expect(new TextEncoder().encode(body).byteLength).toBeLessThanOrEqual(512 * 1024);

    expect(
      renderDirectCacheAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        container: document.getElementById('fictional-slot')!,
        fetcher: async () => corsResponse(body),
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(true);
    await vi.waitFor(() =>
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'cache_invalid_response',
      })
    );
    expect(document.querySelector('iframe')).toBeNull();
    document.body.innerHTML = '';
  });

  it('cancels an oversized streamed body before publishing a failure', async () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);
    const oversized = cacheResponse(new Uint8Array(512 * 1024 + 1));

    expect(
      renderDirectCacheAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        container: document.getElementById('fictional-slot')!,
        fetcher: async () => oversized.response,
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(true);
    await vi.waitFor(() =>
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'cache_invalid_response',
      })
    );

    expect(oversized.cancel).toHaveBeenCalledOnce();
    expect(document.querySelector('iframe')).toBeNull();
    document.body.innerHTML = '';
  });

  it('requires one frozen exact policy and canonical bounded cache source before fetching', () => {
    const cases = [
      {
        policy: { ...CACHE_POLICY },
        source: CACHE_SOURCE,
      },
      {
        policy: CACHE_POLICY,
        source: Object.freeze({
          ...CACHE_SOURCE,
          fetchUrl: `${CACHE_SOURCE.fetchUrl}&uuid=${CACHE_ID}`,
        }),
      },
      {
        policy: CACHE_POLICY,
        source: Object.freeze({
          ...CACHE_SOURCE,
          fetchUrl: `https://other.example/cache?uuid=${CACHE_ID}`,
        }),
      },
      {
        policy: CACHE_POLICY,
        source: Object.freeze({
          ...CACHE_SOURCE,
          fetchUrl: `https://user:password@cache.example:8443/pbc/v1/cache?uuid=${CACHE_ID}`,
        }),
      },
      {
        policy: CACHE_POLICY,
        source: Object.freeze({
          ...CACHE_SOURCE,
          fetchUrl: `${CACHE_SOURCE.fetchUrl}#fragment`,
        }),
      },
      {
        policy: CACHE_POLICY,
        source: Object.freeze({
          ...CACHE_SOURCE,
          fetchUrl: `https://cache.example:9443/pbc/v1/cache?uuid=${CACHE_ID}`,
        }),
      },
      {
        policy: CACHE_POLICY,
        source: Object.freeze({
          ...CACHE_SOURCE,
          fetchUrl: `https://cache.example:8443/pbc/v1/other?uuid=${CACHE_ID}`,
        }),
      },
      {
        policy: CACHE_POLICY,
        source: Object.freeze({
          ...CACHE_SOURCE,
          fetchUrl: `https://cache.example:8443/pbc/v1/cache?id=${CACHE_ID}`,
        }),
      },
      {
        policy: CACHE_POLICY,
        source: Object.freeze({
          ...CACHE_SOURCE,
          fetchUrl: `https://cache.example:8443/${'x'.repeat(4096)}?uuid=${CACHE_ID}`,
        }),
      },
      {
        policy: CACHE_POLICY,
        source: Object.freeze({
          ...CACHE_SOURCE,
          fetchUrl: `${CACHE_SOURCE.fetchUrl}\n`,
        }),
      },
    ];

    for (let index = 0; index < cases.length; index += 1) {
      document.body.innerHTML = `<div id="fictional-slot-${index}"></div>`;
      const candidate = cases[index]!;
      const render = attempt(owner(indexedAttemptId(index), `fictional-slot-${index}`), {
        prepareRenderSource: (value) => (value === candidate.source ? candidate.source : undefined),
      });
      expect(render.admitDirectWinner(candidate.source, WINNER_CONTEXT)).toBe(true);
      const fetchCache = vi.fn();

      expect(
        renderDirectCacheAttempt({
          attempt: render,
          cachePolicy: candidate.policy,
          container: document.getElementById(`fictional-slot-${index}`)!,
          fetcher: fetchCache,
          prepareIframe: prepareAdmIframe,
          publisherOrigin: window.location.origin,
        })
      ).toBe(false);
      expect(fetchCache).not.toHaveBeenCalled();
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'descriptor_invalid',
      });
    }
    document.body.innerHTML = '';
  });

  it.each([-1, 0, 1] as const)(
    'enforces the 4,096-byte canonical fetch URL boundary at delta %s',
    async (delta) => {
      const query = `?uuid=${CACHE_ID}`;
      const prefix = 'https://cache.example/';
      const targetFetchBytes = 4_096 + delta;
      const pathLength =
        targetFetchBytes -
        new TextEncoder().encode(prefix).byteLength -
        new TextEncoder().encode(query).byteLength;
      const policy = Object.freeze({
        version: 1 as const,
        baseUrl: `${prefix}${'x'.repeat(pathLength)}`,
      });
      const source = Object.freeze({
        ...CACHE_SOURCE,
        fetchUrl: `${policy.baseUrl}${query}`,
      });
      expect(new TextEncoder().encode(source.fetchUrl)).toHaveLength(targetFetchBytes);
      document.body.innerHTML = '<div id="url-boundary-slot"></div>';
      const render = attempt(owner(indexedAttemptId(500 + delta), 'url-boundary-slot'), {
        prepareRenderSource: (candidate) => (candidate === source ? source : undefined),
      });
      expect(render.admitDirectWinner(source, WINNER_CONTEXT)).toBe(true);
      const fetchCache = vi.fn(async () => {
        throw new Error('boundary transport stop');
      });
      const started = renderDirectCacheAttempt({
        attempt: render,
        cachePolicy: policy,
        container: document.getElementById('url-boundary-slot')!,
        fetcher: fetchCache,
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      });

      if (delta <= 0) {
        expect(started).toBe(true);
        expect(fetchCache).toHaveBeenCalledOnce();
        await vi.waitFor(() =>
          expect(render.snapshot().outcome).toEqual({
            outcome: 'failed',
            reason: 'cache_network_error',
          })
        );
      } else {
        expect(started).toBe(false);
        expect(fetchCache).not.toHaveBeenCalled();
        expect(render.snapshot().outcome).toEqual({
          outcome: 'failed',
          reason: 'descriptor_invalid',
        });
      }
      document.body.innerHTML = '';
    }
  );

  it.each(['timeout', 'caller cancellation'] as const)(
    'cancels an active body reader after one chunk on %s and ignores its late chunk',
    async (settlement) => {
      vi.useFakeTimers();
      document.body.innerHTML = '<div id="fictional-slot"></div>';
      const render = attempt();
      expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);
      let resolveRead: ((value: { done: boolean; value: Uint8Array }) => void) | undefined;
      const cancel = vi.fn(async () => undefined);
      let reads = 0;
      const read = vi.fn(() => {
        reads += 1;
        if (reads === 1) {
          return Promise.resolve({
            done: false,
            value: new TextEncoder().encode('{"adm":"first chunk'),
          });
        }
        return new Promise<{ done: boolean; value: Uint8Array }>((resolve) => {
          resolveRead = resolve;
        });
      });
      const response = Object.freeze({
        body: Object.freeze({
          getReader: () =>
            Object.freeze({
              cancel,
              read,
              releaseLock: vi.fn(),
            }),
        }),
        ok: true,
        type: 'cors' as const,
      }) as unknown as Response;

      try {
        expect(
          renderDirectCacheAttempt({
            attempt: render,
            cachePolicy: CACHE_POLICY,
            container: document.getElementById('fictional-slot')!,
            fetcher: async () => response,
            prepareIframe: prepareAdmIframe,
            publisherOrigin: window.location.origin,
          })
        ).toBe(true);
        for (let index = 0; index < 5 && read.mock.calls.length < 2; index += 1) {
          await Promise.resolve();
        }
        expect(read).toHaveBeenCalledTimes(2);

        if (settlement === 'timeout') await vi.advanceTimersByTimeAsync(5_000);
        else expect(render.cancel('caller_aborted')).toBe(true);

        expect(cancel).toHaveBeenCalledOnce();
        expect(render.snapshot().outcome).toEqual(
          settlement === 'timeout'
            ? { outcome: 'failed', reason: 'cache_network_error' }
            : { outcome: 'cancelled', reason: 'caller_aborted' }
        );
        resolveRead?.({ done: false, value: new TextEncoder().encode('{"adm":"late"}') });
        await Promise.resolve();
        await Promise.resolve();
        expect(document.querySelector('iframe')).toBeNull();
        expect(cancel).toHaveBeenCalledOnce();
      } finally {
        vi.useRealTimers();
        document.body.innerHTML = '';
      }
    }
  );

  it('aborts the cache request after five seconds and makes late work inert', async () => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);
    let signal: AbortSignal | undefined;
    let resolveFetch: ((response: Response) => void) | undefined;
    const fetchCache = vi.fn((_input: string, init: RequestInit) => {
      signal = init.signal as AbortSignal;
      return new Promise<Response>((resolve) => {
        resolveFetch = resolve;
      });
    });

    try {
      expect(
        renderDirectCacheAttempt({
          attempt: render,
          cachePolicy: CACHE_POLICY,
          container: document.getElementById('fictional-slot')!,
          fetcher: fetchCache,
          prepareIframe: prepareAdmIframe,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      await vi.advanceTimersByTimeAsync(4_999);
      expect(signal?.aborted).toBe(false);
      expect(render.snapshot().outcome).toBeUndefined();
      await vi.advanceTimersByTimeAsync(1);
      expect(signal?.aborted).toBe(true);
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'cache_network_error',
      });

      resolveFetch?.(corsResponse(JSON.stringify({ adm: '<div>late</div>' })));
      await vi.runAllTimersAsync();
      expect(document.querySelector('iframe')).toBeNull();
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'cache_network_error',
      });
    } finally {
      vi.useRealTimers();
      document.body.innerHTML = '';
    }
  });

  it('aborts on caller cancellation and ignores a late cache response', async () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(CACHE_SOURCE, WINNER_CONTEXT)).toBe(true);
    let signal: AbortSignal | undefined;
    let resolveFetch: ((response: Response) => void) | undefined;
    const fetchCache = vi.fn((_input: string, init: RequestInit) => {
      signal = init.signal as AbortSignal;
      return new Promise<Response>((resolve) => {
        resolveFetch = resolve;
      });
    });

    expect(
      renderDirectCacheAttempt({
        attempt: render,
        cachePolicy: CACHE_POLICY,
        container: document.getElementById('fictional-slot')!,
        fetcher: fetchCache,
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(true);
    expect(render.cancel('caller_aborted')).toBe(true);
    expect(signal?.aborted).toBe(true);
    resolveFetch?.(corsResponse(JSON.stringify({ adm: '<div>late</div>' })));
    await Promise.resolve();
    await Promise.resolve();
    expect(document.querySelector('iframe')).toBeNull();
    expect(render.snapshot().outcome).toEqual({ outcome: 'cancelled', reason: 'caller_aborted' });
    document.body.innerHTML = '';
  });
});

function slotOperation(options: SlotOperationOptions): SlotOperation {
  const result = createSlotOperation(options);
  expect(result).toMatchObject({ ok: true });
  if (!result.ok) throw new Error('should create a slot operation');
  return result.value;
}

describe('direct ADM attempt rendering', () => {
  it('accepts the exact intended srcdoc and promotes its iframe artifact', () => {
    document.body.innerHTML = '<div id="fictional-slot"><span>placeholder</span></div>';
    const artifacts = createCommittedArtifactStore();
    const render = attempt(owner(), { artifacts });
    expect(render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
    const container = document.getElementById('fictional-slot')!;

    expect(
      renderDirectAdmAttempt({
        attempt: render,
        container,
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(true);
    expect(render.snapshot()).toMatchObject({ outcome: undefined, state: 'waiting_for_adm' });
    const frame = container.querySelector('iframe');
    expect(frame).not.toBeNull();
    expect(frame?.srcdoc).toContain('fictional creative');
    expect(frame?.hasAttribute('src')).toBe(false);
    expect(container.querySelector('span')).not.toBeNull();

    frame?.dispatchEvent(new Event('load'));
    expect(render.snapshot().outcome).toEqual({ outcome: 'accepted' });
    expect(container.querySelector('span')).toBeNull();
    expect(container.querySelector('iframe')).toBe(frame);
    expect(artifacts.current('fictional-slot')).toMatchObject({
      attemptId: render.id,
      kind: 'direct_iframe',
    });

    artifacts.dispose();
    expect(frame?.isConnected).toBe(false);
    document.body.innerHTML = '';
  });

  it('commits predecessors despite settlement-time iterator poisoning', () => {
    document.body.innerHTML = '<div id="fictional-slot"><span>placeholder</span></div>';
    const container = document.getElementById('fictional-slot')!;
    const predecessor = container.querySelector('span');
    const render = attempt();
    expect(render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
    const iteratorDescriptor = Object.getOwnPropertyDescriptor(Array.prototype, Symbol.iterator);
    const nativeIterator = Array.prototype[Symbol.iterator];
    let ownedIteratorCalls = 0;
    expect(iteratorDescriptor).toBeDefined();
    expect(
      render.onSettled(() => {
        Object.defineProperty(Array.prototype, Symbol.iterator, {
          ...iteratorDescriptor,
          value: function (this: unknown[]) {
            const first = this[0];
            const isAttributeTuple =
              this.length === 2 &&
              typeof first === 'string' &&
              (first === 'sandbox' ||
                first === 'referrerpolicy' ||
                first === 'width' ||
                first === 'height' ||
                first === 'scrolling' ||
                first === 'frameborder' ||
                first === 'marginwidth' ||
                first === 'marginheight' ||
                first === 'title' ||
                first === 'aria-label' ||
                first === 'style');
            const isAttributeList =
              this.length === 11 && Array.isArray(first) && first[0] === 'sandbox';
            const isPredecessorSnapshot = this.length === 1 && first === predecessor;
            if (isAttributeTuple || isAttributeList || isPredecessorSnapshot) {
              ownedIteratorCalls += 1;
              throw new Error('hostile owned-array iterator');
            }
            return Reflect.apply(nativeIterator, this, []);
          },
        });
      })
    ).toBe(true);
    expect(
      renderDirectAdmAttempt({
        attempt: render,
        container,
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(true);
    const frame = container.querySelector('iframe');
    expect(frame).not.toBeNull();

    try {
      frame?.dispatchEvent(new Event('load'));
    } finally {
      if (iteratorDescriptor) {
        Object.defineProperty(Array.prototype, Symbol.iterator, iteratorDescriptor);
      }
    }

    expect(render.snapshot().outcome).toEqual({ outcome: 'accepted' });
    expect(ownedIteratorCalls).toBe(0);
    expect(predecessor?.isConnected).toBe(false);
    expect(frame?.isConnected).toBe(true);
    document.body.innerHTML = '';
  });

  it.each(['property', 'append', 'current', 'activate'] as const)(
    'contains a throwing ADM handle %s phase and disposes its exact frame',
    (phase) => {
      document.body.innerHTML = '<div id="fictional-slot"></div>';
      const container = document.getElementById('fictional-slot')!;
      const render = attempt();
      expect(render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
      let underlying: DirectAdmIframeHandle | undefined;
      const prepareIframe: DirectAdmIframeConstructor = (options) => {
        underlying = prepareAdmIframe(options);
        if (!underlying) return undefined;
        if (phase === 'property') {
          return new Proxy(underlying, {
            get(target, property, receiver) {
              if (property === 'append') throw new Error('hostile append property');
              return Reflect.get(target, property, receiver);
            },
          });
        }
        return Object.freeze({
          frame: underlying.frame,
          append: () => {
            const appended = underlying?.append() === true;
            if (phase === 'append') throw new Error('hostile append');
            return appended;
          },
          activate: () => {
            const activated = underlying?.activate() === true;
            if (phase === 'activate') throw new Error('hostile activate');
            return activated;
          },
          commit: () => underlying?.commit() === true,
          current: () => {
            if (phase === 'current') throw new Error('hostile current');
            return underlying?.current() === true;
          },
          dispose: () => underlying?.dispose(),
        });
      };

      expect(() =>
        renderDirectAdmAttempt({
          attempt: render,
          container,
          prepareIframe,
          publisherOrigin: window.location.origin,
        })
      ).not.toThrow();
      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'adm_document_no_load',
      });
      expect(container.querySelector('iframe')).toBeNull();
      expect(underlying?.append()).toBe(false);
      document.body.innerHTML = '';
    }
  );

  it('rejects a non-publisher creative origin before inserting a frame', () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
    const container = document.getElementById('fictional-slot')!;

    expect(
      renderDirectAdmAttempt({
        attempt: render,
        container,
        prepareIframe: prepareAdmIframe,
        publisherOrigin: 'https://not-the-publisher.example',
      })
    ).toBe(false);
    expect(container.querySelector('iframe')).toBeNull();
    expect(render.snapshot().outcome).toEqual({
      outcome: 'failed',
      reason: 'winner_not_renderable',
    });
    document.body.innerHTML = '';
  });

  it('anchors the five-second deadline after inserting a complete srcdoc frame', () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt(owner(), {
      scheduler: Object.freeze({
        clear: vi.fn(),
        set: (callback: () => void) => {
          callback();
          return Object.freeze({});
        },
      }),
    });
    expect(render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
    const container = document.getElementById('fictional-slot')!;
    const observer = new MutationObserver(() => undefined);
    observer.observe(container, { childList: true });

    expect(
      renderDirectAdmAttempt({
        attempt: render,
        container,
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(false);
    expect(render.snapshot().outcome).toEqual({
      outcome: 'failed',
      reason: 'adm_document_no_load',
    });
    const mutations = observer.takeRecords();
    const inserted = mutations
      .flatMap((mutation) => [...mutation.addedNodes])
      .find((node): node is HTMLIFrameElement => node instanceof HTMLIFrameElement);
    expect(inserted?.srcdoc).toContain('fictional creative');
    expect(inserted?.hasAttribute('src')).toBe(false);
    expect(mutations.some((mutation) => mutation.removedNodes.length === 1)).toBe(true);
    expect(container.querySelector('iframe')).toBeNull();
    observer.disconnect();
    document.body.innerHTML = '';
  });

  it.each(['error', 'removed', 'replaced-srcdoc'] as const)(
    'fails and removes an unaccepted frame when it is %s',
    (failure) => {
      document.body.innerHTML = '<div id="fictional-slot"></div>';
      const render = attempt();
      expect(render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
      const container = document.getElementById('fictional-slot')!;
      expect(
        renderDirectAdmAttempt({
          attempt: render,
          container,
          prepareIframe: prepareAdmIframe,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      const frame = container.querySelector('iframe');
      expect(frame).not.toBeNull();
      if (!frame) throw new Error('should insert an ADM frame');

      if (failure === 'error') frame.dispatchEvent(new Event('error'));
      if (failure === 'removed') {
        frame.remove();
        frame.dispatchEvent(new Event('load'));
      }
      if (failure === 'replaced-srcdoc') {
        frame.srcdoc = '<!doctype html><title>publisher replacement</title>';
        frame.dispatchEvent(new Event('load'));
      }

      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'adm_document_no_load',
      });
      expect(frame.isConnected).toBe(false);
      document.body.innerHTML = '';
    }
  );

  it.each([
    ['sandbox', (frame: HTMLIFrameElement) => frame.setAttribute('sandbox', 'allow-scripts')],
    [
      'referrer policy',
      (frame: HTMLIFrameElement) => frame.setAttribute('referrerpolicy', 'unsafe-url'),
    ],
    ['dimensions', (frame: HTMLIFrameElement) => frame.setAttribute('width', '301')],
    ['layout style', (frame: HTMLIFrameElement) => frame.style.setProperty('width', '301px')],
  ] as const)(
    'refuses acceptance after publisher mutation of the exact %s contract',
    (_field, mutate) => {
      document.body.innerHTML = '<div id="fictional-slot"></div>';
      const render = attempt();
      expect(render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
      const container = document.getElementById('fictional-slot')!;
      expect(
        renderDirectAdmAttempt({
          attempt: render,
          container,
          prepareIframe: prepareAdmIframe,
          publisherOrigin: window.location.origin,
        })
      ).toBe(true);
      const frame = container.querySelector('iframe');
      expect(frame).not.toBeNull();
      if (!frame) throw new Error('should insert an ADM frame');

      mutate(frame);
      frame.dispatchEvent(new Event('load'));

      expect(render.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'adm_document_no_load',
      });
      expect(frame.isConnected).toBe(false);
      document.body.innerHTML = '';
    }
  );

  it('removes on cancellation and makes every late frame event inert', () => {
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt();
    expect(render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
    const container = document.getElementById('fictional-slot')!;
    expect(
      renderDirectAdmAttempt({
        attempt: render,
        container,
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(true);
    const frame = container.querySelector('iframe');
    expect(frame).not.toBeNull();

    expect(render.cancel('caller_aborted')).toBe(true);
    expect(frame?.isConnected).toBe(false);
    frame?.dispatchEvent(new Event('load'));
    frame?.dispatchEvent(new Event('error'));
    expect(render.snapshot().outcome).toEqual({
      outcome: 'cancelled',
      reason: 'caller_aborted',
    });
    document.body.innerHTML = '';
  });

  it('rejects an admitted but malformed frozen ADM source before DOM mutation', () => {
    const malformed = Object.freeze({
      type: 'adm' as const,
      version: 1 as const,
      adm: '',
      width: 0,
      height: 250,
    });
    document.body.innerHTML = '<div id="fictional-slot"></div>';
    const render = attempt(owner(), {
      prepareRenderSource: (candidate) => (candidate === malformed ? malformed : undefined),
    });
    expect(render.admitDirectWinner(malformed, WINNER_CONTEXT)).toBe(true);
    const container = document.getElementById('fictional-slot')!;

    expect(
      renderDirectAdmAttempt({
        attempt: render,
        container,
        prepareIframe: prepareAdmIframe,
        publisherOrigin: window.location.origin,
      })
    ).toBe(false);
    expect(container.querySelector('iframe')).toBeNull();
    expect(render.snapshot().outcome).toEqual({
      outcome: 'failed',
      reason: 'winner_not_renderable',
    });
    document.body.innerHTML = '';
  });
});

describe('RenderAttempt state machine', () => {
  it('implements the exact PUC APS state table and makes invalid/replay transitions inert', () => {
    const scope = owner();
    const candidate = artifact(scope, 'puc');
    const render = attempt(scope);
    const observed: RenderAttemptState[] = [];

    expect(render.beginGamClaim()).toBe(true);
    expect(render.beginDirect()).toBe(false);
    expect(render.admitClaimedWinner(claimed(render, scope, APS_SOURCE))).toBe(true);
    expect(render.ownerClaimed()).toBe(true);
    expect(render.ownerRegistered()).toBe(true);
    expect(render.beginApsDocument(candidate)).toBe(true);
    expect(render.beginAdm(candidate)).toBe(false);
    expect(render.apsDocumentAccepted()).toBe(true);
    expect(render.accept()).toBe(true);
    expect(render.accept()).toBe(false);
    expect(render.fail('runner_failed')).toBe(false);
    expect(candidate.dispose).not.toHaveBeenCalled();

    for (const state of render.snapshot().history) observed.push(state);
    expect(observed).toEqual([
      'created',
      'waiting_for_gam_and_claim',
      'waiting_for_owner',
      'waiting_for_insertion',
      'waiting_for_document',
      'waiting_for_aps_completion',
      'accepted',
    ]);
    expect(render.snapshot()).toMatchObject({
      state: 'accepted',
      outcome: { outcome: 'accepted' },
    });
    expect(scope.disposed).toBe(true);
  });

  it('implements direct and owner ADM paths without permitting APS-only transitions', () => {
    const directOwner = owner();
    const directArtifact = artifact(directOwner);
    const direct = attempt(directOwner);
    expect(direct.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
    expect(direct.beginDirect()).toBe(true);
    expect(direct.beginAdm(directArtifact)).toBe(true);
    expect(direct.apsDocumentAccepted()).toBe(false);
    expect(direct.accept()).toBe(true);

    const pucOwner = owner(ATTEMPT_TWO);
    const pucArtifact = artifact(pucOwner, 'puc');
    const puc = attempt(pucOwner);
    expect(puc.beginGamClaim()).toBe(true);
    expect(puc.admitClaimedWinner(claimed(puc, pucOwner, ADM_SOURCE))).toBe(true);
    expect(puc.ownerClaimed()).toBe(true);
    expect(puc.ownerRegistered()).toBe(true);
    expect(puc.beginAdm(pucArtifact)).toBe(true);
    expect(puc.accept()).toBe(true);
    expect(puc.snapshot().history).toEqual([
      'created',
      'waiting_for_gam_and_claim',
      'waiting_for_owner',
      'waiting_for_insertion',
      'waiting_for_adm',
      'accepted',
    ]);
  });

  it('rejects source and artifact combinations from a different render path', () => {
    const directApsOwner = owner();
    const directAps = attempt(directApsOwner);
    expect(directAps.admitDirectWinner(APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    expect(directAps.beginDirect()).toBe(true);
    expect(directAps.beginAdm(artifact(directApsOwner))).toBe(false);
    expect(directAps.beginApsDocument(artifact(directApsOwner, 'puc'))).toBe(false);
    expect(directAps.beginApsDocument(artifact(directApsOwner))).toBe(true);

    const directAdmOwner = owner(ATTEMPT_TWO);
    const directAdm = attempt(directAdmOwner);
    expect(directAdm.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
    expect(directAdm.beginDirect()).toBe(true);
    expect(directAdm.beginApsDocument(artifact(directAdmOwner))).toBe(false);
    expect(directAdm.beginAdm(artifact(directAdmOwner, 'puc'))).toBe(false);
    expect(directAdm.beginAdm(artifact(directAdmOwner))).toBe(true);

    const pucApsOwner = owner('a1_0000000000000000000002');
    const pucAps = attempt(pucApsOwner);
    expect(pucAps.beginGamClaim()).toBe(true);
    expect(pucAps.admitClaimedWinner(claimed(pucAps, pucApsOwner, APS_SOURCE))).toBe(true);
    expect(pucAps.ownerClaimed()).toBe(true);
    expect(pucAps.ownerRegistered()).toBe(true);
    expect(pucAps.beginAdm(artifact(pucApsOwner, 'puc'))).toBe(false);
    expect(pucAps.beginApsDocument(artifact(pucApsOwner))).toBe(false);
    expect(pucAps.beginApsDocument(artifact(pucApsOwner, 'puc'))).toBe(true);
  });

  it('admits a claimed winner only through the exact one-shot source/context claim', () => {
    const scope = owner();
    const render = attempt(scope);
    expect(render.beginGamClaim()).toBe(true);
    const exactClaim = claimed(render, scope, APS_SOURCE);
    const exactContext = scope.winnerContext;
    if (!exactContext) throw new Error('should admit the exact reservation context');

    const mismatchedOwner = owner(ATTEMPT_TWO);
    const mismatched = attempt(mismatchedOwner);
    expect(mismatched.beginGamClaim()).toBe(true);
    mismatchedOwner.admitClaimedContext(exactContext);
    expect(mismatched.admitClaimedWinner(exactClaim)).toBe(false);
    expect(mismatched.renderSource).toBeUndefined();
    mismatched.cancel('caller_aborted');

    expect(render.admitClaimedWinner(Object.freeze({}))).toBe(false);
    expect(render.admitClaimedWinner(exactClaim)).toBe(true);
    expect(render.renderSource).toEqual(APS_SOURCE);
    expect(render.winnerContext).toBe(exactContext);
    expect(render.admitClaimedWinner(exactClaim)).toBe(false);
  });

  it('enforces every valid, invalid, and replay transition in the state table', () => {
    type Transition =
      | 'admit_direct'
      | 'admit_claimed'
      | 'begin_gam_claim'
      | 'owner_claimed'
      | 'owner_registered'
      | 'begin_direct'
      | 'begin_aps_document'
      | 'begin_adm'
      | 'aps_document_accepted'
      | 'accept'
      | 'no_bid'
      | 'gam_empty'
      | 'fail'
      | 'cancel';
    type ScenarioName =
      | 'created'
      | 'created_direct'
      | 'waiting_for_gam_and_claim'
      | 'waiting_for_gam_and_claim_admitted'
      | 'waiting_for_owner'
      | 'waiting_for_insertion_aps'
      | 'waiting_for_insertion_adm'
      | 'rendering_direct_aps'
      | 'rendering_direct_adm'
      | 'waiting_for_document'
      | 'waiting_for_aps_completion'
      | 'waiting_for_adm'
      | 'accepted'
      | 'no_bid'
      | 'failed'
      | 'cancelled';

    const transitions: readonly Transition[] = [
      'admit_direct',
      'admit_claimed',
      'begin_gam_claim',
      'owner_claimed',
      'owner_registered',
      'begin_direct',
      'begin_aps_document',
      'begin_adm',
      'aps_document_accepted',
      'accept',
      'no_bid',
      'gam_empty',
      'fail',
      'cancel',
    ];
    const valid = new Map<ScenarioName, ReadonlySet<Transition>>([
      ['created', new Set(['admit_direct', 'begin_gam_claim', 'no_bid', 'fail', 'cancel'])],
      ['created_direct', new Set(['begin_direct', 'fail', 'cancel'])],
      ['waiting_for_gam_and_claim', new Set(['admit_claimed', 'gam_empty', 'fail', 'cancel'])],
      [
        'waiting_for_gam_and_claim_admitted',
        new Set(['owner_claimed', 'gam_empty', 'fail', 'cancel']),
      ],
      ['waiting_for_owner', new Set(['owner_registered', 'fail', 'cancel'])],
      ['waiting_for_insertion_aps', new Set(['begin_aps_document', 'fail', 'cancel'])],
      ['waiting_for_insertion_adm', new Set(['begin_adm', 'fail', 'cancel'])],
      ['rendering_direct_aps', new Set(['begin_aps_document', 'fail', 'cancel'])],
      ['rendering_direct_adm', new Set(['begin_adm', 'fail', 'cancel'])],
      ['waiting_for_document', new Set(['aps_document_accepted', 'fail', 'cancel'])],
      ['waiting_for_aps_completion', new Set(['accept', 'fail', 'cancel'])],
      ['waiting_for_adm', new Set(['accept', 'fail', 'cancel'])],
      ['accepted', new Set()],
      ['no_bid', new Set()],
      ['failed', new Set()],
      ['cancelled', new Set()],
    ]);

    const build = (name: ScenarioName): RenderAttempt => {
      const scope = owner();
      const render = attempt(scope);
      const claim = (source: typeof APS_SOURCE | typeof ADM_SOURCE): void => {
        render.beginGamClaim();
        render.admitClaimedWinner(claimed(render, scope, source));
      };
      switch (name) {
        case 'created':
          break;
        case 'created_direct':
          render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
          break;
        case 'waiting_for_gam_and_claim':
          render.beginGamClaim();
          matrixClaims.set(render, claimed(render, scope, APS_SOURCE));
          break;
        case 'waiting_for_gam_and_claim_admitted':
          claim(APS_SOURCE);
          break;
        case 'waiting_for_owner':
          claim(APS_SOURCE);
          render.ownerClaimed();
          break;
        case 'waiting_for_insertion_aps':
          claim(APS_SOURCE);
          render.ownerClaimed();
          render.ownerRegistered();
          break;
        case 'waiting_for_insertion_adm':
          claim(ADM_SOURCE);
          render.ownerClaimed();
          render.ownerRegistered();
          break;
        case 'rendering_direct_aps':
          render.admitDirectWinner(APS_SOURCE, WINNER_CONTEXT);
          render.beginDirect();
          break;
        case 'rendering_direct_adm':
          render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
          render.beginDirect();
          break;
        case 'waiting_for_document':
          render.admitDirectWinner(APS_SOURCE, WINNER_CONTEXT);
          render.beginDirect();
          render.beginApsDocument(artifact(scope));
          break;
        case 'waiting_for_aps_completion':
          render.admitDirectWinner(APS_SOURCE, WINNER_CONTEXT);
          render.beginDirect();
          render.beginApsDocument(artifact(scope));
          render.apsDocumentAccepted();
          break;
        case 'waiting_for_adm':
          render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
          render.beginDirect();
          render.beginAdm(artifact(scope));
          break;
        case 'accepted':
          render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
          render.beginDirect();
          render.beginAdm(artifact(scope));
          render.accept();
          break;
        case 'no_bid':
          render.noBid();
          break;
        case 'failed':
          render.fail('internal_error');
          break;
        case 'cancelled':
          render.cancel('caller_aborted');
          break;
      }
      return render;
    };

    const invoke = (render: RenderAttempt, transition: Transition): boolean => {
      const kind = render.snapshot().state === 'waiting_for_insertion' ? 'puc' : 'direct_iframe';
      switch (transition) {
        case 'admit_direct':
          return render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
        case 'admit_claimed':
          return render.admitClaimedWinner(matrixClaims.get(render) ?? Object.freeze({}));
        case 'begin_gam_claim':
          return render.beginGamClaim();
        case 'owner_claimed':
          return render.ownerClaimed();
        case 'owner_registered':
          return render.ownerRegistered();
        case 'begin_direct':
          return render.beginDirect();
        case 'begin_aps_document':
          return render.beginApsDocument(artifact(render, kind));
        case 'begin_adm':
          return render.beginAdm(artifact(render, kind));
        case 'aps_document_accepted':
          return render.apsDocumentAccepted();
        case 'accept':
          return render.accept();
        case 'no_bid':
          return render.noBid();
        case 'gam_empty':
          return render.fail('gam_empty');
        case 'fail':
          return render.fail('internal_error');
        case 'cancel':
          return render.cancel('caller_aborted');
      }
    };

    for (const [scenario, expectedTransitions] of valid) {
      for (const transition of transitions) {
        const render = build(scenario);
        const expected = expectedTransitions.has(transition);
        expect(invoke(render, transition), `${scenario} -> ${transition}`).toBe(expected);
        if (expected) {
          expect(invoke(render, transition), `${scenario} -> ${transition} replay`).toBe(false);
        }
        if (!render.snapshot().outcome) render.cancel('caller_aborted');
      }
    }
  });

  it('owns the exact admitted source and winner context for a direct APS path', () => {
    const scope = owner();
    const render = attempt(scope);
    expect(render.beginDirect()).toBe(false);
    expect(render.admitDirectWinner(APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    expect(render.renderSource).toBe(APS_SOURCE);
    expect(render.winnerContext).toBe(WINNER_CONTEXT);
    expect(render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(false);

    const candidate = artifact(scope);
    expect(render.beginDirect()).toBe(true);
    expect(render.beginApsDocument(candidate)).toBe(true);
    expect(render.apsDocumentAccepted()).toBe(true);
    expect(render.accept()).toBe(true);
    expect(render.renderSource).toBeUndefined();
    expect(render.winnerContext).toBeUndefined();
  });

  it('allows no_bid only for the exact parsed decision before rendering starts', () => {
    const noBid = attempt();
    expect(noBid.noBid()).toBe(true);
    expect(noBid.snapshot()).toMatchObject({ state: 'no_bid', outcome: { outcome: 'no_bid' } });
    expect(noBid.beginDirect()).toBe(false);

    const rendering = attempt(owner(ATTEMPT_TWO));
    expect(rendering.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
    expect(rendering.beginDirect()).toBe(true);
    expect(rendering.noBid()).toBe(false);
    expect(rendering.fail('invalid_response')).toBe(true);
  });

  it('races state-owned timeout, success, failure, abort, and navigation disposal through one latch', () => {
    vi.useFakeTimers();
    try {
      const timedOwner = owner();
      const timedArtifact = artifact(timedOwner);
      const timed = attempt(timedOwner, {
        owner: timedOwner,
        artifacts: createCommittedArtifactStore(),
      });
      expect(timed.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
      expect(timed.beginDirect()).toBe(true);
      expect(timed.beginAdm(timedArtifact)).toBe(true);
      vi.advanceTimersByTime(5_000);
      expect(timed.snapshot()).toMatchObject({
        outcome: { outcome: 'failed', reason: 'adm_document_no_load' },
      });
      expect(timedArtifact.dispose).toHaveBeenCalledOnce();
      expect(timed.accept()).toBe(false);
      expect(timed.cancel('caller_aborted')).toBe(false);

      const aborted = attempt(owner(ATTEMPT_TWO));
      expect(aborted.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
      expect(aborted.beginDirect()).toBe(true);
      expect(aborted.cancel('caller_aborted')).toBe(true);
      expect(aborted.fail('internal_error')).toBe(false);

      const navigationOwner = owner('a1_0000000000000000000002');
      const navigationAttempt = attempt(navigationOwner);
      expect(navigationAttempt.beginGamClaim()).toBe(true);
      navigationOwner.disposeFromNavigation();
      expect(navigationAttempt.snapshot()).toMatchObject({
        outcome: { outcome: 'cancelled', reason: 'navigation_disposed' },
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it('uses fixed transition-owned deadline timings and failure mappings', () => {
    vi.useFakeTimers();
    try {
      const registrationOwner = owner();
      const registration = attempt(registrationOwner);
      registration.beginGamClaim();
      registration.admitClaimedWinner(claimed(registration, registrationOwner, APS_SOURCE));
      registration.ownerClaimed();
      vi.advanceTimersByTime(2_999);
      expect(registration.snapshot().state).toBe('waiting_for_owner');
      vi.advanceTimersByTime(1);
      expect(registration.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'owner_registration_timeout',
      });

      const insertionOwner = owner(ATTEMPT_TWO);
      const insertion = attempt(insertionOwner);
      insertion.beginGamClaim();
      insertion.admitClaimedWinner(claimed(insertion, insertionOwner, APS_SOURCE));
      insertion.ownerClaimed();
      insertion.ownerRegistered();
      vi.advanceTimersByTime(1_000);
      expect(insertion.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'owner_insertion_timeout',
      });

      const documentOwner = owner('a1_0000000000000000000002');
      const documentAttempt = attempt(documentOwner);
      documentAttempt.admitDirectWinner(APS_SOURCE, WINNER_CONTEXT);
      documentAttempt.beginDirect();
      documentAttempt.beginApsDocument(artifact(documentOwner));
      vi.advanceTimersByTime(3_000);
      expect(documentAttempt.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'renderer_document_no_load',
      });

      const completionOwner = owner('a1_0000000000000000000003');
      const completion = attempt(completionOwner);
      completion.admitDirectWinner(APS_SOURCE, WINNER_CONTEXT);
      completion.beginDirect();
      completion.beginApsDocument(artifact(completionOwner));
      completion.apsDocumentAccepted();
      vi.advanceTimersByTime(10_000);
      expect(completion.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'runner_failed',
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it('reserves transition and terminal latches before hostile scheduler and artifact cleanup', () => {
    const transitionReference: { current?: RenderAttempt } = {};
    let clearReenters = false;
    const scheduler = {
      set: vi.fn(() => Object.freeze({})),
      clear: vi.fn(() => {
        if (clearReenters) transitionReference.current?.cancel('caller_aborted');
      }),
    };
    const transitionOwner = owner();
    const transitionAttempt = attempt(transitionOwner, { scheduler });
    transitionReference.current = transitionAttempt;
    transitionAttempt.admitDirectWinner(APS_SOURCE, WINNER_CONTEXT);
    transitionAttempt.beginDirect();
    transitionAttempt.beginApsDocument(artifact(transitionOwner));
    clearReenters = true;

    expect(transitionAttempt.apsDocumentAccepted()).toBe(true);
    expect(transitionAttempt.snapshot().outcome).toEqual({
      outcome: 'cancelled',
      reason: 'caller_aborted',
    });
    expect(transitionAttempt.snapshot().history.slice(-2)).toEqual([
      'waiting_for_aps_completion',
      'cancelled',
    ]);

    const disposalReference: { current?: RenderAttempt } = {};
    const disposalOwner = owner(ATTEMPT_TWO);
    const hostileArtifact = Object.freeze({
      kind: 'direct_iframe' as const,
      attemptId: disposalOwner.id,
      slot: disposalOwner.slot,
      navigationGeneration: disposalOwner.navigationGeneration,
      dispose: vi.fn(() => disposalReference.current?.cancel('superseded')),
    });
    const disposalAttempt = attempt(disposalOwner);
    disposalReference.current = disposalAttempt;
    disposalAttempt.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
    disposalAttempt.beginDirect();
    disposalAttempt.beginAdm(hostileArtifact);

    expect(disposalAttempt.fail('internal_error')).toBe(true);
    expect(disposalAttempt.snapshot().outcome).toEqual({
      outcome: 'failed',
      reason: 'internal_error',
    });
    expect(disposalAttempt.snapshot().history.filter((state) => state === 'failed')).toHaveLength(
      1
    );
    expect(disposalAttempt.snapshot().history).not.toContain('cancelled');
  });

  it('does not promote after deadline cleanup reentrantly settles the attempt', () => {
    const artifacts = createCommittedArtifactStore();
    const reference: { current?: RenderAttempt } = {};
    let cancelOnClear = false;
    const scheduler = {
      set: vi.fn(() => Object.freeze({})),
      clear: vi.fn(() => {
        if (cancelOnClear) reference.current?.cancel('caller_aborted');
      }),
    };
    const scope = owner();
    const candidate = artifact(scope);
    const render = attempt(scope, { artifacts, scheduler });
    reference.current = render;
    render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
    render.beginDirect();
    render.beginAdm(candidate);
    cancelOnClear = true;

    expect(render.accept()).toBe(false);
    expect(render.snapshot().outcome).toEqual({
      outcome: 'cancelled',
      reason: 'caller_aborted',
    });
    expect(candidate.dispose).toHaveBeenCalledOnce();
    expect(artifacts.current(scope.slot)).toBeUndefined();
  });

  it('rejects malformed or stale attempt ownership before registering work', () => {
    const malformed = owner('bad-attempt');
    expect(
      createRenderAttempt({
        owner: malformed,
        artifacts: createCommittedArtifactStore(),
        reservations: reservations(),
        prepareRenderSource,
      })
    ).toEqual({ ok: false, reason: 'invalid_attempt' });

    const stale = owner();
    stale.disposeFromNavigation();
    expect(
      createRenderAttempt({
        owner: stale,
        artifacts: createCommittedArtifactStore(),
        reservations: reservations(),
        prepareRenderSource,
      })
    ).toEqual({ ok: false, reason: 'stale_owner' });
  });

  it('transactionally disposes owners when lifecycle registration cannot commit', () => {
    for (const mode of ['throw', 'callback', 'identity'] as const) {
      const scope = owner();
      const originalDispose = scope.dispose;
      const dispose = vi.fn(() => originalDispose());
      Object.defineProperty(scope, 'dispose', { configurable: true, value: dispose });
      Object.defineProperty(scope, 'onDispose', {
        configurable: true,
        value: (_kind: string, callback: () => void) => {
          if (mode === 'callback') callback();
          if (mode === 'identity') {
            Object.defineProperty(scope, 'id', { configurable: true, value: ATTEMPT_TWO });
          }
          if (mode === 'throw') throw new Error('registration failed');
        },
      });

      expect(
        createRenderAttempt({
          owner: scope,
          artifacts: createCommittedArtifactStore(),
          reservations: reservations(),
          prepareRenderSource,
        })
      ).toEqual({ ok: false, reason: 'stale_owner' });
      expect(dispose, mode).toHaveBeenCalledOnce();
    }
  });

  it('releases real session indexes after every post-issuance construction rejection', () => {
    let issuedByte = 0;
    const runtime = createRuntimeSession({
      createIdentityIssuer: () =>
        createTestNavigationIdentityIssuer({
          getRandomValues: (target) => {
            target.fill(issuedByte);
            issuedByte += 1;
            return target;
          },
        }),
    });
    const navigation = runtime.startInitialNavigation();
    if (!navigation.ok) throw new Error('should start a navigation');
    const batch = navigation.value.createAuctionBatch('batch-render-construction');
    if (!batch) throw new Error('should create an auction batch');
    const slot = 'fictional-slot';

    const unbrandedOwner = batch.createRenderAttempt(slot);
    if (!unbrandedOwner.ok) throw new Error('should issue the first owner');
    expect(
      createRenderAttempt({
        owner: unbrandedOwner.value,
        artifacts: { ...createCommittedArtifactStore() },
        reservations: reservations(),
        prepareRenderSource,
      })
    ).toEqual({ ok: false, reason: 'invalid_attempt' });

    const invalidSchedulerOwner = batch.createRenderAttempt(slot);
    expect(invalidSchedulerOwner).toMatchObject({ ok: true });
    if (!invalidSchedulerOwner.ok) throw new Error('should retry after provenance rejection');
    expect(
      createRenderAttempt({
        owner: invalidSchedulerOwner.value,
        artifacts: createCommittedArtifactStore(),
        reservations: reservations(),
        prepareRenderSource,
        scheduler: { set: undefined as never, clear: () => undefined },
      })
    ).toEqual({ ok: false, reason: 'invalid_attempt' });

    const unbrandedReservationsOwner = batch.createRenderAttempt(slot);
    expect(unbrandedReservationsOwner).toMatchObject({ ok: true });
    if (!unbrandedReservationsOwner.ok) {
      throw new Error('should retry after scheduler rejection');
    }
    expect(
      createRenderAttempt({
        owner: unbrandedReservationsOwner.value,
        artifacts: createCommittedArtifactStore(),
        prepareRenderSource,
        reservations: {
          ...reservations(),
          consumeClaim: () =>
            Object.freeze({
              renderSource: ADM_SOURCE,
              winnerContext: WINNER_CONTEXT,
            }),
        },
      })
    ).toEqual({ ok: false, reason: 'invalid_attempt' });

    expect(batch.createRenderAttempt(slot)).toMatchObject({ ok: true });
    runtime.dispose();
  });

  it('runtime-rejects invalid terminal reasons instead of publishing malformed outcomes', () => {
    const render = attempt();
    expect(render.fail('invented_failure' as never)).toBe(false);
    expect(render.cancel('invented_cancellation' as never)).toBe(false);
    expect(render.snapshot()).toMatchObject({ state: 'created', outcome: undefined });
    expect(render.fail('internal_error')).toBe(true);
  });
});

describe('committed artifact ownership', () => {
  it('promotes before attempt disposal, preserves accepted DOM, and disposes the prior artifact before replacement', () => {
    const store = createCommittedArtifactStore();
    const generation = Object.freeze({});
    const firstOwner = owner(ATTEMPT_ONE, 'fictional-slot', generation);
    const firstArtifact = artifact(firstOwner);
    const first = attempt(firstOwner, { owner: firstOwner, artifacts: store });
    first.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
    first.beginDirect();
    first.beginAdm(firstArtifact);
    expect(first.accept()).toBe(true);
    expect(firstOwner.disposed).toBe(true);
    expect(firstArtifact.dispose).not.toHaveBeenCalled();
    expect(store.current('fictional-slot')).toBe(firstArtifact);

    const secondOwner = owner(ATTEMPT_TWO, 'fictional-slot', generation);
    const secondArtifact = artifact(secondOwner);
    const second = attempt(secondOwner, { owner: secondOwner, artifacts: store });
    second.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
    second.beginDirect();
    second.beginAdm(secondArtifact);
    expect(second.accept()).toBe(true);
    expect(firstArtifact.dispose).toHaveBeenCalledOnce();
    expect(store.current('fictional-slot')).toBe(secondArtifact);
    expect(secondArtifact.dispose).not.toHaveBeenCalled();

    store.disposeNavigation(generation);
    expect(secondArtifact.dispose).toHaveBeenCalledOnce();
    expect(store.current('fictional-slot')).toBeUndefined();
  });

  it('disposes only uncommitted artifacts on failure or cancellation', () => {
    for (const [index, settle] of (['failed', 'cancelled'] as const).entries()) {
      const scope = owner(`a1_000000000000000000000${index}`);
      const candidate = artifact(scope);
      const render = attempt(scope);
      render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
      render.beginDirect();
      render.beginAdm(candidate);
      if (settle === 'failed') expect(render.fail('adm_document_no_load')).toBe(true);
      else expect(render.cancel('superseded')).toBe(true);
      expect(candidate.dispose).toHaveBeenCalledOnce();
    }
  });

  it('does not publish a replacement when prior-artifact disposal reentrantly cancels it', () => {
    const store = createCommittedArtifactStore();
    const generation = Object.freeze({});
    const secondOwner = owner(ATTEMPT_TWO, 'fictional-slot', generation);
    const firstOwner = owner(ATTEMPT_ONE, 'fictional-slot', generation);
    const firstArtifact = Object.freeze({
      kind: 'direct_iframe' as const,
      attemptId: firstOwner.id,
      slot: 'fictional-slot',
      navigationGeneration: generation,
      dispose: vi.fn(() => secondOwner.disposeFromNavigation()),
    });
    const first = attempt(firstOwner, { owner: firstOwner, artifacts: store });
    first.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
    first.beginDirect();
    first.beginAdm(firstArtifact);
    first.accept();

    const secondArtifact = artifact(secondOwner);
    const second = attempt(secondOwner, { owner: secondOwner, artifacts: store });
    second.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
    second.beginDirect();
    second.beginAdm(secondArtifact);

    expect(second.accept()).toBe(false);
    expect(second.snapshot()).toMatchObject({
      outcome: { outcome: 'cancelled', reason: 'navigation_disposed' },
    });
    expect(firstArtifact.dispose).toHaveBeenCalledOnce();
    expect(secondArtifact.dispose).toHaveBeenCalledOnce();
    expect(store.current('fictional-slot')).toBeUndefined();
  });

  it('requires an immutable exact-attempt artifact without invoking accessors', () => {
    const scope = owner();
    const render = attempt(scope);
    render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
    render.beginDirect();
    const wrongAttempt = Object.freeze({
      ...artifact(scope),
      attemptId: ATTEMPT_TWO,
    });
    expect(render.beginAdm(wrongAttempt)).toBe(false);

    const getter = vi.fn(() => 'direct_iframe');
    const hostile = Object.freeze(
      Object.defineProperties(
        {},
        {
          attemptId: { enumerable: true, value: scope.id },
          dispose: { enumerable: true, value: vi.fn() },
          kind: { enumerable: true, get: getter },
          navigationGeneration: { enumerable: true, value: scope.navigationGeneration },
          slot: { enumerable: true, value: scope.slot },
        }
      )
    );
    expect(render.beginAdm(hostile as CommittedRenderArtifact)).toBe(false);
    expect(getter).not.toHaveBeenCalled();
  });

  it('defers reentrant navigation disposal and never publishes into a disposed generation', () => {
    const store = createCommittedArtifactStore();
    const generation = Object.freeze({});
    const firstOwner = owner(ATTEMPT_ONE, 'slot-one', generation);
    const secondOwner = owner(ATTEMPT_TWO, 'slot-two', generation);
    const replacementOwner = owner('a1_0000000000000000000002', 'slot-one', generation);
    const first = Object.freeze({
      kind: 'direct_iframe' as const,
      attemptId: firstOwner.id,
      slot: firstOwner.slot,
      navigationGeneration: generation,
      dispose: vi.fn(() => store.disposeNavigation(generation)),
    });
    const second = artifact(secondOwner);
    const replacement = artifact(replacementOwner);
    expect(store.promote(first)).toBe(true);
    expect(store.promote(second)).toBe(true);

    expect(store.promote(replacement)).toBe(false);
    expect(first.dispose).toHaveBeenCalledOnce();
    expect(second.dispose).toHaveBeenCalledOnce();
    expect(store.current('slot-one')).toBeUndefined();
    expect(store.current('slot-two')).toBeUndefined();
  });

  it('never retries a throwing artifact disposer', () => {
    const store = createCommittedArtifactStore();
    const generation = Object.freeze({});
    const firstOwner = owner(ATTEMPT_ONE, 'fictional-slot', generation);
    const replacementOwner = owner(ATTEMPT_TWO, 'fictional-slot', generation);
    const dispose = vi.fn(() => {
      throw new Error('partial artifact disposal');
    });
    const first = Object.freeze({
      kind: 'direct_iframe' as const,
      attemptId: firstOwner.id,
      slot: firstOwner.slot,
      navigationGeneration: generation,
      dispose,
    });
    expect(store.promote(first)).toBe(true);
    expect(store.promote(artifact(replacementOwner))).toBe(false);
    expect(store.current('fictional-slot')).toBe(first);

    store.disposeNavigation(generation);
    expect(dispose).toHaveBeenCalledOnce();
    expect(store.current('fictional-slot')).toBeUndefined();
  });

  it('fails closed and contains an asynchronous artifact disposer', async () => {
    const store = createCommittedArtifactStore();
    const generation = Object.freeze({});
    const firstOwner = owner(ATTEMPT_ONE, 'fictional-slot', generation);
    const replacementOwner = owner(ATTEMPT_TWO, 'fictional-slot', generation);
    const dispose = vi.fn(async () => {
      throw new Error('asynchronous artifact disposal is unsupported');
    });
    const first = Object.freeze({
      kind: 'direct_iframe' as const,
      attemptId: firstOwner.id,
      slot: firstOwner.slot,
      navigationGeneration: generation,
      dispose,
    });
    expect(store.promote(first)).toBe(true);

    expect(store.promote(artifact(replacementOwner))).toBe(false);
    await Promise.resolve();

    expect(dispose).toHaveBeenCalledOnce();
    expect(store.current('fictional-slot')).toBe(first);
  });

  it.each(['fulfilled_promise', 'fulfilling_thenable'] as const)(
    'contains an asynchronous %s disposer without publishing a replacement',
    async (mode) => {
      const store = createCommittedArtifactStore();
      const generation = Object.freeze({});
      const firstOwner = owner(ATTEMPT_ONE, 'fictional-slot', generation);
      const replacementOwner = owner(ATTEMPT_TWO, 'fictional-slot', generation);
      const dispose = vi.fn(() =>
        mode === 'fulfilled_promise'
          ? Promise.resolve()
          : {
              then: (fulfilled: () => void) => {
                queueMicrotask(() => fulfilled());
              },
            }
      );
      const first = Object.freeze({
        kind: 'direct_iframe' as const,
        attemptId: firstOwner.id,
        slot: firstOwner.slot,
        navigationGeneration: generation,
        dispose,
      });
      expect(store.promote(first)).toBe(true);

      expect(store.promote(artifact(replacementOwner))).toBe(false);
      await Promise.resolve();
      await Promise.resolve();

      expect(dispose).toHaveBeenCalledOnce();
      expect(store.current('fictional-slot')).toBe(first);
    }
  );

  it('never republishes an artifact after its disposal has started', () => {
    const store = createCommittedArtifactStore();
    const candidate = artifact(owner());
    expect(store.promote(candidate)).toBe(true);
    expect(store.release(candidate)).toBe(true);
    expect(candidate.dispose).toHaveBeenCalledOnce();

    expect(store.promote(candidate)).toBe(false);
    expect(store.current(candidate.slot)).toBeUndefined();
    expect(candidate.dispose).toHaveBeenCalledOnce();
  });

  it('preserves the prior artifact when promotion currentness is already false', () => {
    const store = createCommittedArtifactStore();
    const generation = Object.freeze({});
    const current = artifact(owner(ATTEMPT_ONE, 'fictional-slot', generation));
    const candidate = artifact(owner(ATTEMPT_TWO, 'fictional-slot', generation));
    expect(store.promote(current)).toBe(true);

    expect(store.promote(candidate, () => false)).toBe(false);
    expect(current.dispose).not.toHaveBeenCalled();
    expect(candidate.dispose).not.toHaveBeenCalled();
    expect(store.current('fictional-slot')).toBe(current);
  });

  it('never publishes after its navigation generation or whole store is disposed', () => {
    const generation = Object.freeze({});
    const navigationStore = createCommittedArtifactStore();
    const navigationArtifact = artifact(owner(ATTEMPT_ONE, 'fictional-slot', generation));
    navigationStore.disposeNavigation(generation);
    expect(navigationStore.promote(navigationArtifact)).toBe(false);
    expect(navigationStore.current('fictional-slot')).toBeUndefined();

    const runtimeStore = createCommittedArtifactStore();
    const runtimeArtifact = artifact(owner(ATTEMPT_TWO, 'fictional-slot', generation));
    expect(
      runtimeStore.promote(runtimeArtifact, () => {
        runtimeStore.dispose();
        return true;
      })
    ).toBe(false);
    expect(runtimeStore.current('fictional-slot')).toBeUndefined();
  });

  it('contains collection prototype tampering at every artifact-store boundary', () => {
    const store = createCommittedArtifactStore();
    const generation = Object.freeze({});
    const first = artifact(owner(ATTEMPT_ONE, 'fictional-slot', generation));
    const replacement = artifact(owner(ATTEMPT_TWO, 'fictional-slot', generation));
    const originalMapGet = Map.prototype.get;
    const originalSetAdd = Set.prototype.add;
    const originalWeakMapHas = WeakMap.prototype.has;

    let promoted: boolean | undefined;
    Map.prototype.get = () => {
      throw new Error('tampered Map.get');
    };
    try {
      promoted = store.promote(first);
    } finally {
      Map.prototype.get = originalMapGet;
    }
    expect(promoted).toBe(true);

    let released: boolean | undefined;
    WeakMap.prototype.has = () => {
      throw new Error('tampered WeakMap.has');
    };
    try {
      released = store.release(first);
    } finally {
      WeakMap.prototype.has = originalWeakMapHas;
    }
    expect(released).toBe(true);
    expect(first.dispose).toHaveBeenCalledOnce();

    expect(store.promote(replacement)).toBe(true);
    Set.prototype.add = () => {
      throw new Error('tampered Set.add');
    };
    try {
      store.disposeNavigation(generation);
    } finally {
      Set.prototype.add = originalSetAdd;
    }
    expect(replacement.dispose).toHaveBeenCalledOnce();
    expect(store.current('fictional-slot')).toBeUndefined();
  });

  it('keeps store bookkeeping valid when a disposer tampers with collection prototypes', () => {
    const store = createCommittedArtifactStore();
    const generation = Object.freeze({});
    const originalMapGet = Map.prototype.get;
    const dispose = vi.fn(() => {
      Map.prototype.get = () => {
        throw new Error('tampered Map.get');
      };
    });
    const current = Object.freeze({
      kind: 'direct_iframe' as const,
      attemptId: ATTEMPT_ONE,
      slot: 'fictional-slot',
      navigationGeneration: generation,
      dispose,
    });
    const replacement = artifact(owner(ATTEMPT_TWO, 'fictional-slot', generation));
    expect(store.promote(current)).toBe(true);
    let promoted: boolean | undefined;
    try {
      promoted = store.promote(replacement);
    } finally {
      Map.prototype.get = originalMapGet;
    }

    expect(promoted).toBe(true);
    expect(dispose).toHaveBeenCalledOnce();
    expect(store.current('fictional-slot')).toBe(replacement);
  });
});

describe('RenderAttempt diagnostics producer', () => {
  it('publishes one frozen terminal observation only after accepted artifact state commits', () => {
    const artifacts = createCommittedArtifactStore();
    const attemptReference: { current?: RenderAttempt } = {};
    const snapshots: RenderAttemptSnapshot[] = [];
    const publishDiagnostics = vi.fn((observation: RenderAttemptDiagnosticsObservation) => {
      expect(Object.isFrozen(observation)).toBe(true);
      expect(Object.isFrozen(observation.outcome)).toBe(true);
      snapshots.push(attemptReference.current!.snapshot());
      throw new Error('fictional diagnostics failure');
    });
    const renderAttempt = attempt(owner(), { artifacts, publishDiagnostics });
    attemptReference.current = renderAttempt;
    expect(renderAttempt.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
    expect(renderAttempt.beginDirect()).toBe(true);
    const committed = artifact(renderAttempt);
    expect(renderAttempt.beginAdm(committed)).toBe(true);

    expect(renderAttempt.accept()).toBe(true);

    expect(artifacts.current(renderAttempt.slot)).toBe(committed);
    expect(snapshots).toEqual([
      expect.objectContaining({ state: 'accepted', outcome: { outcome: 'accepted' } }),
    ]);
    expect(publishDiagnostics).toHaveBeenCalledOnce();
    expect(publishDiagnostics).toHaveBeenCalledWith({
      kind: 'render_attempt',
      attemptId: renderAttempt.id,
      slotId: renderAttempt.slot,
      path: 'auction',
      rendered: true,
      injected: true,
      servedFrom: 'inline',
      state: 'accepted',
      outcome: { outcome: 'accepted' },
    });
  });

  it('publishes source-owned APS trace identity without exposing the creative payload', () => {
    const publishDiagnostics = vi.fn();
    const renderAttempt = attempt(owner(), { publishDiagnostics });
    expect(renderAttempt.admitDirectWinner(DIRECT_APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    expect(renderAttempt.beginDirect()).toBe(true);
    const committed = artifact(renderAttempt);
    expect(renderAttempt.beginApsDocument(committed)).toBe(true);
    expect(renderAttempt.apsDocumentAccepted()).toBe(true);

    expect(renderAttempt.accept()).toBe(true);

    expect(publishDiagnostics).toHaveBeenCalledWith(
      expect.objectContaining({
        bidId: DIRECT_APS_SOURCE.bidId,
        creativeId: DIRECT_APS_SOURCE.creativeId,
        injected: true,
        rendered: true,
      })
    );
    const observation = publishDiagnostics.mock.calls[0]?.[0] as Record<string, unknown>;
    expect(observation).not.toHaveProperty('aaxResponse');
    expect(observation).not.toHaveProperty('creativeUrl');
  });

  it('publishes terminal failure after the lifecycle state commit and never republishes', () => {
    const attemptReference: { current?: RenderAttempt } = {};
    const observedStates: RenderAttemptState[] = [];
    const publishDiagnostics = vi.fn(() => {
      observedStates.push(attemptReference.current!.snapshot().state);
      return false;
    });
    const renderAttempt = attempt(owner(), { publishDiagnostics });
    attemptReference.current = renderAttempt;

    expect(renderAttempt.fail('runner_failed')).toBe(true);
    expect(renderAttempt.cancel('superseded')).toBe(false);

    expect(observedStates).toEqual(['failed']);
    expect(publishDiagnostics).toHaveBeenCalledOnce();
  });
});

describe('SlotOperation result isolation', () => {
  it('rejects an unbranded structural primary before observing or starting fallback', () => {
    const createFallback = vi.fn();
    const forged = {
      id: ATTEMPT_ONE,
      slot: 'fictional-slot',
      navigationGeneration: Object.freeze({}),
      onSettled: vi.fn(),
      snapshot: vi.fn(),
    } as unknown as RenderAttempt;

    expect(createSlotOperation({ primary: forged, createFallback })).toEqual({
      ok: false,
      reason: 'invalid_attempt',
    });
    expect(forged.onSettled).not.toHaveBeenCalled();
    expect(createFallback).not.toHaveBeenCalled();
  });

  it('retains immutable primary gam_empty and settles from one distinct fallback child', () => {
    const primary = attempt();
    let fallback: RenderAttempt | undefined;
    const operation = slotOperation({
      primary,
      createFallback: (parentAttemptId) => {
        const childOwner = owner(ATTEMPT_TWO, primary.slot, primary.navigationGeneration);
        const result = createRenderAttempt({
          owner: childOwner,
          artifacts: createCommittedArtifactStore(),
          reservations: reservations(),
          prepareRenderSource,
          parentAttemptId,
        });
        if (result.ok) fallback = result.value;
        return result;
      },
    });

    primary.beginGamClaim();
    expect(primary.fail('gam_empty')).toBe(true);
    expect(fallback).toBeDefined();
    expect(fallback?.parentAttemptId).toBe(primary.id);
    expect(fallback?.id).not.toBe(primary.id);
    expect(fallback?.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
    fallback?.beginDirect();
    const fallbackArtifact = artifact(fallback!);
    fallback?.beginAdm(fallbackArtifact);
    expect(fallback?.accept()).toBe(true);

    expect(operation.snapshot()).toEqual({
      settled: true,
      result: {
        path: 'fallback',
        outcome: { outcome: 'accepted' },
        primaryAttemptId: ATTEMPT_ONE,
        primary: { outcome: 'failed', reason: 'gam_empty' },
        fallbackAttemptId: ATTEMPT_TWO,
        fallback: { outcome: 'accepted' },
      },
    });
    expect(Object.isFrozen(operation.snapshot().result)).toBe(true);
  });

  it('does not start fallback for ineligible primary results or settle twice', () => {
    const primary = attempt();
    const createFallback = vi.fn();
    const operation = slotOperation({ primary, createFallback });
    primary.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
    primary.beginDirect();
    primary.fail('runner_failed');

    expect(createFallback).not.toHaveBeenCalled();
    expect(operation.snapshot()).toMatchObject({
      settled: true,
      result: {
        path: 'primary',
        outcome: { outcome: 'failed', reason: 'runner_failed' },
      },
    });
    expect(primary.cancel('superseded')).toBe(false);
  });

  it('cannot forge fallback with gam_empty outside an attributable GAM state', () => {
    const primary = attempt();
    const createFallback = vi.fn();
    const operation = slotOperation({ primary, createFallback });

    expect(primary.fail('gam_empty')).toBe(false);
    expect(createFallback).not.toHaveBeenCalled();
    expect(operation.snapshot()).toEqual({ settled: false });
    expect(primary.cancel('caller_aborted')).toBe(true);
  });

  it('rejects a fallback child from another navigation generation', () => {
    const primary = attempt();
    let child: RenderAttempt | undefined;
    const operation = slotOperation({
      primary,
      createFallback: (parentAttemptId) => {
        const result = createRenderAttempt({
          owner: owner(ATTEMPT_TWO),
          artifacts: createCommittedArtifactStore(),
          reservations: reservations(),
          prepareRenderSource,
          parentAttemptId,
        });
        if (result.ok) child = result.value;
        return result;
      },
    });
    primary.beginGamClaim();
    primary.fail('gam_empty');

    expect(child?.snapshot().outcome).toEqual({
      outcome: 'cancelled',
      reason: 'superseded',
    });
    expect(operation.snapshot()).toMatchObject({
      settled: true,
      result: {
        path: 'fallback',
        outcome: { outcome: 'failed', reason: 'internal_error' },
      },
    });
  });

  it('fails closed when fallback identity issuance fails', () => {
    const primary = attempt();
    const operation = slotOperation({
      primary,
      createFallback: () => Object.freeze({ ok: false, reason: 'identity_generation_failed' }),
    });
    primary.beginGamClaim();
    primary.fail('gam_empty');

    expect(operation.snapshot()).toMatchObject({
      settled: true,
      result: {
        path: 'fallback',
        primary: { outcome: 'failed', reason: 'gam_empty' },
        outcome: { outcome: 'failed', reason: 'identity_generation_failed' },
      },
    });
  });

  it('contains hostile fallback result getters and child subscription failures', () => {
    const getterPrimary = attempt();
    const getterOperation = slotOperation({
      primary: getterPrimary,
      createFallback: () =>
        Object.defineProperty({}, 'ok', {
          get: () => {
            throw new Error('hostile result getter');
          },
        }) as never,
    });
    getterPrimary.beginGamClaim();
    getterPrimary.fail('gam_empty');
    expect(getterOperation.snapshot()).toMatchObject({
      settled: true,
      result: { outcome: { outcome: 'failed', reason: 'internal_error' } },
    });

    const subscriptionPrimary = attempt(owner(ATTEMPT_ONE, 'fictional-slot', Object.freeze({})));
    const hostileChild = {
      id: ATTEMPT_TWO,
      slot: subscriptionPrimary.slot,
      parentAttemptId: subscriptionPrimary.id,
      navigationGeneration: subscriptionPrimary.navigationGeneration,
      cancel: vi.fn(() => true),
      onSettled: () => {
        throw new Error('hostile child subscription');
      },
    } as unknown as RenderAttempt;
    const subscriptionOperation = slotOperation({
      primary: subscriptionPrimary,
      createFallback: () => Object.freeze({ ok: true, value: hostileChild }),
    });
    subscriptionPrimary.beginGamClaim();
    subscriptionPrimary.fail('gam_empty');
    expect(subscriptionOperation.snapshot()).toMatchObject({
      settled: true,
      result: { outcome: { outcome: 'failed', reason: 'internal_error' } },
    });
    expect(hostileChild.cancel).toHaveBeenCalledOnce();
  });

  it('rejects a fallback result accessor without rereading or cancelling another value', () => {
    const primary = attempt();
    const first = { cancel: vi.fn() };
    const second = { cancel: vi.fn() };
    let reads = 0;
    const result = Object.freeze(
      Object.defineProperties(
        {},
        {
          ok: { enumerable: true, value: true },
          value: {
            enumerable: true,
            get: () => {
              reads += 1;
              return reads === 1 ? first : second;
            },
          },
        }
      )
    );
    const operation = slotOperation({ primary, createFallback: () => result as never });
    primary.beginGamClaim();
    primary.fail('gam_empty');

    expect(operation.snapshot()).toMatchObject({
      settled: true,
      result: { outcome: { outcome: 'failed', reason: 'internal_error' } },
    });
    expect(reads).toBe(0);
    expect(first.cancel).not.toHaveBeenCalled();
    expect(second.cancel).not.toHaveBeenCalled();
  });
});
