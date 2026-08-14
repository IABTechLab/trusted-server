import { describe, expect, it, vi } from 'vitest';
import { JSDOM } from 'jsdom';

import {
  createBootstrapController,
  createFirstDisplayArtifactController,
  createFirstDisplayBootstrapRuntimeBridge,
  createFirstDisplayTakeoverCoordinator,
  createPersistentRuntimeLoader,
} from '../../src/core/bootstrap_controller';
import { createFirstDisplayHandoffOwner } from '../../src/first_display/handoff';
import type { FirstDisplayAgent } from '../../src/first_display/agent';
import {
  consumeFirstDisplayTakeoverTransport,
  installFirstDisplayTakeoverTransport,
} from '../../src/shared/takeover';
import type { FirstDisplayComponentRegistrationV1 } from '../../src/first_display/registration';
import type { FirstDisplaySliceActivationContext } from '../../src/first_display/transaction';

const RELEASE_ID = 'a'.repeat(64);

function artifactDocument(): {
  document: Document;
  script: HTMLScriptElement;
} {
  const dom = new JSDOM(
    `<!doctype html><script id="trustedserver-js" src="/static/tsjs=tsjs-first-display.min.js?m=0041&v=${'b'.repeat(64)}"></script>`,
    { url: 'https://publisher.example/' }
  );
  const script = dom.window.document.querySelector('script') as HTMLScriptElement;
  Object.defineProperty(dom.window.document, 'currentScript', {
    configurable: true,
    value: script,
  });
  return { document: dom.window.document, script };
}

function component(
  id: string,
  order: number,
  activate: FirstDisplayComponentRegistrationV1['prepare']
): FirstDisplayComponentRegistrationV1 {
  return Object.freeze({ abi: 1, id, releaseId: RELEASE_ID, order, prepare: activate });
}

describe('first-display bootstrap controller', () => {
  it('loads and commits one post-paint runtime through the private takeover bridge', () => {
    const { document, script } = artifactDocument();
    const target = {};
    const events: string[] = [];
    const timers: Array<() => void> = [];
    let now = 0;
    let revision = 0;
    const owner = createFirstDisplayHandoffOwner({
      releaseId: RELEASE_ID,
      generation: 1,
      isCurrentGeneration: () => true,
      isTerminal: () => true,
      isPainted: () => true,
      closeIngress: () => events.push('close'),
      onFailure: () => events.push('owner:failure'),
    });
    const agent = Object.freeze({
      state: 'painted' as const,
      observeNativeMutation: () => {
        const observed = owner.observeMutation();
        revision = owner.mutationRevision;
        return observed;
      },
      finalizeHandoff: () =>
        owner.finalize(() => ({
          candidate: {
            version: 1,
            releaseId: RELEASE_ID,
            generation: 1,
            projectionDigest: 'b'.repeat(64),
            slices: ['first_display'],
            slots: [
              {
                id: 'slot-1',
                aliases: [],
                domId: 'div-1',
                gamPath: '/123/slot-1',
                formats: [[300, 250]],
                owner: 'trusted_server',
                outcome: 'failed',
                targeting: [],
                committedArtifact: 'none',
                gptToken: null,
              },
            ],
            attempts: [
              {
                id: 'a1_AAECAwQFBgcAAAAAAAAAAQ',
                slotId: 'slot-1',
                ordinal: 1,
                state: 'failed',
                reason: 'internal_error',
              },
            ],
            tombstones: [],
            artifacts: [],
            parserState: [],
            gptDiagnostics: { facts: [], overflowCount: 0, dropCount: 0 },
            timing: { bidsScriptMs: 1, firstDisplayMs: null, terminalMs: 2, paintMs: 3 },
            highWater: {
              navigationAttemptPrefix: 'AAECAwQFBgc',
              nextNavigationAttemptOrdinal: 2,
              nextAttemptOrdinal: 2,
              nextSlotRegistrationOrdinal: 2,
              reservationClockEpochMs: 0,
              nextReservationOrdinal: 1,
              nextTicketOrdinal: 1,
            },
            cycles: [],
            trace: {
              nextSequence: 1,
              nextGlobalSlotOrdinal: 2,
              slots: [{ slotId: 'slot-1', impressions: 0, bindings: [] }],
            },
            mutationRevision: revision,
          },
          identities: [],
        })),
      detachCommittedArtifacts: () => true,
      snapshot: () => ({ mutationRevision: revision }),
      dispose: () => events.push('dispose:agent'),
    }) as unknown as FirstDisplayAgent;
    const bridge = createFirstDisplayBootstrapRuntimeBridge({
      target,
      document,
      agentScript: script,
      runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`,
      outline: {
        version: 1,
        releaseId: RELEASE_ID,
        generation: 1,
        projectionDigest: 'b'.repeat(64),
        slices: ['first_display'],
        slotCount: 1,
        outcomeCount: 1,
        capabilities: [],
        objectKinds: [],
      },
      isCurrentGeneration: () => true,
      now: () => now,
      setTimer: (callback) => {
        timers.push(callback);
        return callback;
      },
      clearTimer: (handle) => {
        const index = timers.indexOf(handle as () => void);
        if (index >= 0) timers.splice(index, 1);
      },
      disposeAgentArtifact: () => {
        events.push('dispose:artifact');
        agent.dispose();
      },
      onFailure: (reason) => events.push(`fallback:${reason}`),
    });

    expect(bridge?.bindAgent(agent)).toBe(true);
    expect(document.querySelector('#trustedserver-js-runtime')).toBeNull();
    expect(bridge?.onProtectedPaint()).toBe(true);
    expect(revision).toBe(1);
    expect(timers).toHaveLength(1);
    const transport = consumeFirstDisplayTakeoverTransport(target);
    expect(transport.status).toBe('accepted');
    if (transport.status !== 'accepted') throw new Error('should consume takeover transport');
    transport.coordinate(
      Object.freeze({
        activate: () => events.push('activate'),
        commit: () => events.push('commit'),
        rollback: () => events.push('rollback'),
      })
    );

    now = 10_000;
    expect(bridge?.state).toBe('committed');
    expect(timers).toEqual([]);
    expect(document.querySelector('#trustedserver-js-runtime')).not.toBeNull();
    expect(events).toEqual(['close', 'dispose:artifact', 'dispose:agent', 'activate', 'commit']);
  });

  it('fails the post-paint bridge once at the exact ten-second boundary', () => {
    const { document, script } = artifactDocument();
    const target = {};
    const timers: Array<() => void> = [];
    const failures: string[] = [];
    let now = 0;
    const dispose = vi.fn();
    const agent = Object.freeze({
      state: 'painted' as const,
      observeNativeMutation: () => true,
      finalizeHandoff: () => undefined,
      detachCommittedArtifacts: () => false,
      snapshot: () => ({ mutationRevision: 0 }),
      dispose,
    }) as unknown as FirstDisplayAgent;
    const bridge = createFirstDisplayBootstrapRuntimeBridge({
      target,
      document,
      agentScript: script,
      runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`,
      outline: {},
      isCurrentGeneration: () => true,
      now: () => now,
      setTimer: (callback) => {
        timers.push(callback);
        return callback;
      },
      clearTimer: (handle) => {
        const index = timers.indexOf(handle as () => void);
        if (index >= 0) timers.splice(index, 1);
      },
      disposeAgentArtifact: dispose,
      onFailure: (reason) => failures.push(reason),
    });
    expect(bridge?.bindAgent(agent)).toBe(true);
    expect(bridge?.onProtectedPaint()).toBe(true);

    now = 10_000;
    timers[0]?.();
    expect(bridge?.state).toBe('failed');
    expect(document.querySelector('#trustedserver-js-runtime')).toBeNull();
    expect(Object.prototype.hasOwnProperty.call(target, '_firstDisplayTakeover')).toBe(false);
    expect(dispose).toHaveBeenCalledOnce();
    expect(failures).toEqual(['bundle_partial']);
  });

  it('does not request persistent bytes until invoked and authenticates the one runtime node', () => {
    const { document, script } = artifactDocument();
    script.nonce = 'server-nonce';
    const mutations = vi.fn();
    const failures = vi.fn();
    const loader = createPersistentRuntimeLoader({
      document,
      agentScript: script,
      runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`,
      onMutation: mutations,
      onFailure: failures,
    });
    expect(document.querySelector('#trustedserver-js-runtime')).toBeNull();

    expect(loader.request()).toBe(true);
    const runtime = document.querySelector('#trustedserver-js-runtime') as HTMLScriptElement;
    expect(runtime).toBeInstanceOf(document.defaultView?.HTMLScriptElement);
    expect(runtime.src).toBe(
      `${document.location.origin}/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`
    );
    expect(runtime.nonce).toBe('server-nonce');
    expect(runtime.async).toBe(true);
    expect(loader.authenticate()).toBe(true);
    expect(mutations).toHaveBeenCalledOnce();
    expect(failures).not.toHaveBeenCalled();

    const duplicate = runtime.cloneNode() as HTMLScriptElement;
    document.head.append(duplicate);
    expect(loader.authenticate()).toBe(false);
  });

  it('moves the private takeover callback between IIFEs exactly once', () => {
    const target = {};
    const coordinate = vi.fn();
    const release = installFirstDisplayTakeoverTransport(target, coordinate);
    expect(release).toBeTypeOf('function');
    expect(Object.keys(target)).toEqual([]);
    expect(consumeFirstDisplayTakeoverTransport(target)).toEqual({
      status: 'accepted',
      coordinate,
    });
    expect(consumeFirstDisplayTakeoverTransport(target)).toEqual({ status: 'absent' });
    release?.();
  });

  it('joins the bound painted agent to one prepared persistent transaction and drops it', () => {
    const events: string[] = [];
    const owner = createFirstDisplayHandoffOwner({
      releaseId: RELEASE_ID,
      generation: 1,
      isCurrentGeneration: () => true,
      isTerminal: () => true,
      isPainted: () => true,
      closeIngress: () => events.push('close'),
      onFailure: () => events.push('owner:failure'),
    });
    const finalized = owner.finalize(() => ({
      candidate: {
        version: 1,
        releaseId: RELEASE_ID,
        generation: 1,
        projectionDigest: 'b'.repeat(64),
        slices: ['first_display'],
        slots: [
          {
            id: 'slot-1',
            aliases: [],
            domId: 'div-1',
            gamPath: '/123/slot-1',
            formats: [[300, 250]],
            owner: 'trusted_server',
            outcome: 'failed',
            targeting: [],
            committedArtifact: 'none',
            gptToken: null,
          },
        ],
        attempts: [
          {
            id: 'a1_AAECAwQFBgcAAAAAAAAAAQ',
            slotId: 'slot-1',
            ordinal: 1,
            state: 'failed',
            reason: 'internal_error',
          },
        ],
        tombstones: [],
        artifacts: [],
        parserState: [],
        gptDiagnostics: { facts: [], overflowCount: 0, dropCount: 0 },
        timing: { bidsScriptMs: 1, firstDisplayMs: null, terminalMs: 2, paintMs: 3 },
        highWater: {
          navigationAttemptPrefix: 'AAECAwQFBgc',
          nextNavigationAttemptOrdinal: 2,
          nextAttemptOrdinal: 2,
          nextSlotRegistrationOrdinal: 2,
          reservationClockEpochMs: 0,
          nextReservationOrdinal: 1,
          nextTicketOrdinal: 1,
        },
        cycles: [],
        trace: {
          nextSequence: 1,
          nextGlobalSlotOrdinal: 2,
          slots: [{ slotId: 'slot-1', impressions: 0, bindings: [] }],
        },
        mutationRevision: 0,
      },
      identities: [],
    }));
    expect(finalized).toBeDefined();
    const agent = Object.freeze({
      state: 'painted' as const,
      finalizeHandoff: () => {
        events.push('finalize');
        return finalized;
      },
      detachCommittedArtifacts: () => {
        events.push('detach');
        return true;
      },
      snapshot: () => ({ mutationRevision: 0 }),
      dispose: () => events.push('dispose'),
    }) as unknown as FirstDisplayAgent;
    const coordinator = createFirstDisplayTakeoverCoordinator({
      outline: {
        version: 1,
        releaseId: RELEASE_ID,
        generation: 1,
        projectionDigest: 'b'.repeat(64),
        slices: ['first_display'],
        slotCount: 1,
        outcomeCount: 1,
        capabilities: [],
        objectKinds: [],
      },
      isCurrentGeneration: () => true,
      authenticateRuntimeScript: () => true,
      onFailure: () => events.push('fallback'),
    });
    expect(coordinator.bindAgent(agent)).toBe(true);
    coordinator.coordinateTakeover(
      Object.freeze({
        activate: (adoption?: unknown) => {
          expect(adoption).toMatchObject({ adoptInitialDisplay: true });
          events.push('activate');
        },
        commit: () => events.push('commit'),
        rollback: () => events.push('rollback'),
      })
    );

    expect(coordinator.state).toBe('committed');
    expect(events).toEqual(['close', 'finalize', 'detach', 'dispose', 'activate', 'commit']);
    coordinator.dispose();
    expect(events.filter((event) => event === 'dispose')).toHaveLength(1);
  });

  it('owns the bids mark, one deadline, and exact registration/action transitions', () => {
    let now = 0;
    const marks: string[] = [];
    const deadline = vi.fn();
    const timers: Array<() => void> = [];
    const controller = createBootstrapController({
      performance: { mark: (name) => marks.push(name) },
      now: () => now,
      startedAtMs: 0,
      setTimer: (callback) => {
        timers.push(callback);
        return callback;
      },
      clearTimer: (handle) => {
        const index = timers.indexOf(handle as () => void);
        if (index >= 0) timers.splice(index, 1);
      },
      onFailure: deadline,
    });

    expect(marks).toEqual(['tsjs:bids-script']);
    expect(controller.registerAgent()).toBe(true);
    now = 9_999;
    expect(controller.startAction()).toBe(true);
    controller.settle();
    expect(timers).toEqual([]);
    expect(deadline).not.toHaveBeenCalled();
  });

  it('fails at exactly 10 seconds and never admits a late or duplicate transition', () => {
    let now = 10_000;
    const failures: string[] = [];
    const controller = createBootstrapController({
      performance: { mark: () => undefined },
      now: () => now,
      startedAtMs: 0,
      setTimer: (callback) => callback,
      clearTimer: () => undefined,
      onFailure: (reason) => failures.push(reason),
    });
    expect(controller.registerAgent()).toBe(false);
    expect(controller.startAction()).toBe(false);
    expect(failures).toEqual(['bundle_partial']);
    now = 0;
    expect(controller.registerAgent()).toBe(false);
  });

  it('owns the ephemeral artifact sink and starts only after every selected component activates', () => {
    const { document, script } = artifactDocument();
    const target = {};
    const events: string[] = [];
    const bootstrap = createBootstrapController({
      performance: { mark: () => undefined },
      now: () => 0,
      setTimer: (callback) => callback,
      clearTimer: () => undefined,
      onFailure: (reason) => events.push(`failure:${reason}`),
    });
    const artifact = createFirstDisplayArtifactController({
      bootstrap,
      target,
      document,
      script,
      releaseId: RELEASE_ID,
      generation: 1,
      expectedSliceIds: ['first_display', 'gpt_initial'],
      isCurrentGeneration: () => true,
      baseHost: Object.freeze({}),
    });
    const sink = Object.getOwnPropertyDescriptor(target, '_registerFirstDisplay');
    expect(sink).toMatchObject({ configurable: true, enumerable: false, writable: false });

    expect(
      Reflect.apply(sink?.value, target, [
        component('first_display', 1, () =>
          Object.freeze({
            activate: ({ afterActivate }: FirstDisplaySliceActivationContext) => {
              events.push('activate:base');
              afterActivate(() => events.push('start:action'));
            },
            sliceHost: Object.freeze({ activate: () => undefined }),
          })
        ),
        script,
      ])
    ).toBe(true);
    expect(events).toEqual([]);
    expect(
      Reflect.apply(sink?.value, target, [
        component('gpt_initial', 7, () =>
          Object.freeze({ activate: () => events.push('activate:gpt') })
        ),
        script,
      ])
    ).toBe(true);
    expect(events).toEqual(['activate:base', 'activate:gpt', 'start:action']);
    expect(artifact?.state).toBe('active');
    expect(Object.prototype.hasOwnProperty.call(target, '_registerFirstDisplay')).toBe(false);
  });

  it('fails closed and removes the sink on a wrong-release component', () => {
    const { document, script } = artifactDocument();
    const target = {};
    const failures: string[] = [];
    const bootstrap = createBootstrapController({
      performance: { mark: () => undefined },
      now: () => 0,
      setTimer: (callback) => callback,
      clearTimer: () => undefined,
      onFailure: (reason) => failures.push(reason),
    });
    const artifact = createFirstDisplayArtifactController({
      bootstrap,
      target,
      document,
      script,
      releaseId: RELEASE_ID,
      generation: 1,
      expectedSliceIds: ['first_display'],
      isCurrentGeneration: () => true,
      baseHost: Object.freeze({}),
    });
    const sink = Object.getOwnPropertyDescriptor(target, '_registerFirstDisplay')?.value;
    expect(
      Reflect.apply(sink, target, [
        Object.freeze({
          ...component('first_display', 1, () => Object.freeze({ activate: () => undefined })),
          releaseId: 'b'.repeat(64),
        }),
        script,
      ])
    ).toBe(false);
    expect(artifact?.state).toBe('failed');
    expect(failures).toEqual(['abi_mismatch']);
    expect(Object.prototype.hasOwnProperty.call(target, '_registerFirstDisplay')).toBe(false);
  });
});
