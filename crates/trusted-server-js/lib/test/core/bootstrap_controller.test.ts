import { describe, expect, it, vi } from 'vitest';
import { JSDOM } from 'jsdom';

import {
  createBootstrapController,
  createFirstDisplayArtifactController,
} from '../../src/core/bootstrap_controller';
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
          ...component('first_display', 1, () =>
            Object.freeze({ activate: () => undefined })
          ),
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
