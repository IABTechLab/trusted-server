import { JSDOM } from 'jsdom';
import { describe, expect, it, vi } from 'vitest';

import { createFirstDisplayTransaction } from '../../src/first_display/transaction';

const RELEASE_ID = 'a'.repeat(64);

function documentWithScript(): { document: Document; script: HTMLScriptElement } {
  const dom = new JSDOM(
    '<!doctype html><script id="trustedserver-js" src="https://publisher.example/static/tsjs=tsjs-first-display.min.js?m=0041&v=' +
      'b'.repeat(64) +
      '"></script>',
    { url: 'https://publisher.example/' }
  );
  const document = dom.window.document;
  const script = document.querySelector('script') as HTMLScriptElement;
  Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
  return { document, script };
}

function registration(
  id: string,
  order: number,
  activate: (own: (dispose: () => void) => void) => void = () => undefined
): Record<string, unknown> {
  return {
    abi: 1,
    id,
    releaseId: RELEASE_ID,
    generation: 7,
    order,
    prepare: () => ({
      activate: ({ own }: { own: (dispose: () => void) => void }) => activate(own),
    }),
  };
}

describe('release-private first-display transaction', () => {
  it('accepts exact ordered registrations and activates in one synchronous transaction', () => {
    const { document, script } = documentWithScript();
    const events: string[] = [];
    const transaction = createFirstDisplayTransaction({
      document,
      script,
      releaseId: RELEASE_ID,
      generation: 7,
      expectedSliceIds: ['first_display', 'gpt_initial'],
      isCurrentGeneration: () => true,
    });

    expect(
      transaction.register(
        registration('first_display', 1, (own) => {
          own(() => events.push('dispose-base'));
          events.push('activate-base');
        })
      )
    ).toBe(true);
    expect(
      transaction.register(
        registration('gpt_initial', 2, (own) => {
          own(() => events.push('dispose-gpt'));
          events.push('activate-gpt');
        })
      )
    ).toBe(true);
    expect(transaction.activate()).toBe(true);
    expect(events).toEqual(['activate-base', 'activate-gpt']);
    transaction.dispose();
    expect(events).toEqual(['activate-base', 'activate-gpt', 'dispose-gpt', 'dispose-base']);
  });

  it('rejects unknown, duplicate, omitted, misordered, late, wrong-release, and accessor registrations', () => {
    const make = () => {
      const { document, script } = documentWithScript();
      return createFirstDisplayTransaction({
        document,
        script,
        releaseId: RELEASE_ID,
        generation: 7,
        expectedSliceIds: ['first_display', 'gpt_initial'],
        isCurrentGeneration: () => true,
      });
    };

    expect(make().register(registration('unknown_initial', 1))).toBe(false);
    const duplicate = make();
    expect(duplicate.register(registration('first_display', 1))).toBe(true);
    expect(duplicate.register(registration('first_display', 1))).toBe(false);
    const omitted = make();
    expect(omitted.register(registration('first_display', 1))).toBe(true);
    expect(omitted.activate()).toBe(false);
    expect(make().register(registration('gpt_initial', 2))).toBe(false);
    expect(
      make().register({ ...registration('first_display', 1), releaseId: 'b'.repeat(64) })
    ).toBe(false);
    const accessor = registration('first_display', 1);
    Object.defineProperty(accessor, 'prepare', { enumerable: true, get: vi.fn() });
    expect(make().register(accessor)).toBe(false);
    const late = make();
    expect(late.register(registration('first_display', 1))).toBe(true);
    expect(late.register(registration('gpt_initial', 2))).toBe(true);
    expect(late.activate()).toBe(true);
    expect(late.register(registration('gpt_initial', 2))).toBe(false);
  });

  it('authenticates the exact parser-inserted current script and current generation', () => {
    const { document, script } = documentWithScript();
    const replaced = document.createElement('script');
    document.head.append(replaced);
    const wrongScript = createFirstDisplayTransaction({
      document,
      script: replaced,
      releaseId: RELEASE_ID,
      generation: 7,
      expectedSliceIds: ['first_display'],
      isCurrentGeneration: () => true,
    });
    expect(wrongScript.register(registration('first_display', 1))).toBe(false);

    const stale = createFirstDisplayTransaction({
      document,
      script,
      releaseId: RELEASE_ID,
      generation: 7,
      expectedSliceIds: ['first_display'],
      isCurrentGeneration: () => false,
    });
    expect(stale.register(registration('first_display', 1))).toBe(false);
  });

  it('rolls back every owned effect in reverse order after an activation failure', () => {
    const { document, script } = documentWithScript();
    const events: string[] = [];
    const transaction = createFirstDisplayTransaction({
      document,
      script,
      releaseId: RELEASE_ID,
      generation: 7,
      expectedSliceIds: ['first_display', 'gpt_initial'],
      isCurrentGeneration: () => true,
    });
    transaction.register(
      registration('first_display', 1, (own) => {
        own(() => events.push('dispose-a'));
        own(() => events.push('dispose-b'));
      })
    );
    transaction.register(
      registration('gpt_initial', 2, (own) => {
        own(() => events.push('dispose-c'));
        throw new Error('boom');
      })
    );

    expect(transaction.activate()).toBe(false);
    expect(events).toEqual(['dispose-c', 'dispose-b', 'dispose-a']);
    expect(transaction.state).toBe('failed');
  });
});
