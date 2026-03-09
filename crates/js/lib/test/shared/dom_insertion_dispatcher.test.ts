import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  installDataDomeGuard,
  resetGuardState as resetDataDomeGuardState,
} from '../../src/integrations/datadome/script_guard';
import {
  installGptGuard,
  resetGuardState as resetGptGuardState,
} from '../../src/integrations/gpt/script_guard';
import {
  installGtmGuard,
  resetGuardState as resetGtmGuardState,
} from '../../src/integrations/google_tag_manager/script_guard';
import {
  installNextJsGuard,
  resetGuardState as resetLockrGuardState,
} from '../../src/integrations/lockr/nextjs_guard';
import {
  installPermutiveGuard,
  resetGuardState as resetPermutiveGuardState,
} from '../../src/integrations/permutive/script_guard';
import {
  registerDomInsertionHandler,
  resetDomInsertionDispatcherForTests,
} from '../../src/shared/dom_insertion_dispatcher';
import { createScriptGuard } from '../../src/shared/script_guard';

function resetAllScriptGuards(): void {
  resetDataDomeGuardState();
  resetGptGuardState();
  resetGtmGuardState();
  resetLockrGuardState();
  resetPermutiveGuardState();
}

describe('DOM insertion dispatcher', () => {
  const dispatcherKey = Symbol.for('trusted-server.domInsertionDispatcher');
  let originalAppendChild: typeof Element.prototype.appendChild;
  let originalInsertBefore: typeof Element.prototype.insertBefore;

  beforeEach(() => {
    resetAllScriptGuards();
    resetDomInsertionDispatcherForTests();
    originalAppendChild = Element.prototype.appendChild;
    originalInsertBefore = Element.prototype.insertBefore;
  });

  afterEach(() => {
    resetAllScriptGuards();
    Element.prototype.appendChild = originalAppendChild;
    Element.prototype.insertBefore = originalInsertBefore;
    resetDomInsertionDispatcherForTests();
  });

  it('installs a single shared prototype patch across integrations', () => {
    installNextJsGuard();

    const sharedAppendChild = Element.prototype.appendChild;
    const sharedInsertBefore = Element.prototype.insertBefore;

    expect(sharedAppendChild).not.toBe(originalAppendChild);
    expect(sharedInsertBefore).not.toBe(originalInsertBefore);

    installPermutiveGuard();
    installDataDomeGuard();
    installGtmGuard();
    installGptGuard();

    expect(Element.prototype.appendChild).toBe(sharedAppendChild);
    expect(Element.prototype.insertBefore).toBe(sharedInsertBefore);

    const container = document.createElement('div');

    const lockrScript = document.createElement('script');
    lockrScript.src = 'https://aim.loc.kr/identity-lockr-v1.0.js';
    container.appendChild(lockrScript);
    expect(lockrScript.src).toContain('/integrations/lockr/sdk');

    const permutiveScript = document.createElement('script');
    permutiveScript.src = 'https://cdn.permutive.com/abc123-web.js';
    container.appendChild(permutiveScript);
    expect(permutiveScript.src).toContain('/integrations/permutive/sdk');

    const dataDomeScript = document.createElement('script');
    dataDomeScript.src = 'https://js.datadome.co/tags.js';
    container.appendChild(dataDomeScript);
    expect(dataDomeScript.src).toContain('/integrations/datadome/tags.js');

    const gtmScript = document.createElement('script');
    gtmScript.src = 'https://www.googletagmanager.com/gtm.js?id=GTM-TEST';
    container.appendChild(gtmScript);
    expect(gtmScript.src).toContain('/integrations/google_tag_manager/gtm.js?id=GTM-TEST');

    const gptLink = document.createElement('link');
    gptLink.setAttribute('rel', 'preload');
    gptLink.setAttribute('as', 'script');
    gptLink.href =
      'https://securepubads.g.doubleclick.net/pagead/managed/js/gpt/m202603020101/pubads_impl.js';
    container.appendChild(gptLink);
    expect(gptLink.href).toContain(
      '/integrations/gpt/pagead/managed/js/gpt/m202603020101/pubads_impl.js'
    );
  });

  it('prefers lower-priority handlers when multiple handlers match', () => {
    const calls: string[] = [];

    const unregisterSlower = registerDomInsertionHandler({
      handle: () => {
        calls.push('slower');
        return true;
      },
      id: 'zeta',
      priority: 100,
    });

    const unregisterFaster = registerDomInsertionHandler({
      handle: () => {
        calls.push('faster');
        return true;
      },
      id: 'alpha',
      priority: 50,
    });

    const container = document.createElement('div');
    const script = document.createElement('script');
    script.src = 'https://example.com/priority.js';
    container.appendChild(script);

    expect(calls).toEqual(['faster']);

    unregisterSlower();
    unregisterFaster();
  });

  it('falls back to integration ID ordering when priorities match', () => {
    const calls: string[] = [];

    const unregisterBeta = registerDomInsertionHandler({
      handle: () => {
        calls.push('beta');
        return true;
      },
      id: 'beta',
      priority: 100,
    });

    const unregisterAlpha = registerDomInsertionHandler({
      handle: () => {
        calls.push('alpha');
        return true;
      },
      id: 'alpha',
      priority: 100,
    });

    const container = document.createElement('div');
    const script = document.createElement('script');
    script.src = 'https://example.com/tie-breaker.js';
    container.appendChild(script);

    expect(calls).toEqual(['alpha']);

    unregisterBeta();
    unregisterAlpha();
  });

  it('keeps the shared wrapper installed until the last guard resets', () => {
    const firstGuard = createScriptGuard({
      displayName: 'Alpha',
      id: 'alpha',
      isTargetUrl: (url) => url.includes('alpha.js'),
      proxyPath: '/integrations/alpha/sdk',
    });
    const secondGuard = createScriptGuard({
      displayName: 'Beta',
      id: 'beta',
      isTargetUrl: (url) => url.includes('beta.js'),
      proxyPath: '/integrations/beta/sdk',
    });

    firstGuard.install();
    const sharedAppendChild = Element.prototype.appendChild;
    const sharedInsertBefore = Element.prototype.insertBefore;

    secondGuard.install();
    firstGuard.reset();

    expect(Element.prototype.appendChild).toBe(sharedAppendChild);
    expect(Element.prototype.insertBefore).toBe(sharedInsertBefore);

    secondGuard.reset();

    expect(Element.prototype.appendChild).toBe(originalAppendChild);
    expect(Element.prototype.insertBefore).toBe(originalInsertBefore);
  });

  it('does not clobber external prototype patches when the last handler resets', () => {
    const guard = createScriptGuard({
      displayName: 'Alpha',
      id: 'alpha',
      isTargetUrl: (url) => url.includes('alpha.js'),
      proxyPath: '/integrations/alpha/sdk',
    });

    guard.install();

    const externalAppendChild = vi.fn(function <T extends Node>(this: Element, node: T): T {
      return originalAppendChild.call(this, node) as T;
    });

    Element.prototype.appendChild = externalAppendChild as typeof Element.prototype.appendChild;

    guard.reset();

    expect(Element.prototype.appendChild).toBe(
      externalAppendChild as typeof Element.prototype.appendChild
    );
    expect(Element.prototype.insertBefore).toBe(originalInsertBefore);
  });

  it('skips handlers for text nodes and unrelated elements', () => {
    const handle = vi.fn(() => true);
    const unregister = registerDomInsertionHandler({
      handle,
      id: 'alpha',
      priority: 100,
    });

    const container = document.createElement('div');
    const textNode = document.createTextNode('dispatcher fast path');
    const image = document.createElement('img');
    image.src = 'https://example.com/image.png';

    container.appendChild(textNode);
    container.appendChild(image);

    expect(handle).not.toHaveBeenCalled();
    expect(container.textContent).toContain('dispatcher fast path');
    expect(container.querySelector('img')).toBe(image);

    unregister();
  });

  it('continues dispatching when one handler throws', () => {
    const calls: string[] = [];

    const unregisterThrowing = registerDomInsertionHandler({
      handle: () => {
        calls.push('throwing');
        throw new Error('boom');
      },
      id: 'alpha',
      priority: 50,
    });
    const unregisterSecond = registerDomInsertionHandler({
      handle: () => {
        calls.push('second');
        return true;
      },
      id: 'beta',
      priority: 100,
    });

    const container = document.createElement('div');
    const script = document.createElement('script');
    script.src = 'https://example.com/recover.js';

    expect(() => container.appendChild(script)).not.toThrow();
    expect(calls).toEqual(['throwing', 'second']);
    expect(container.querySelector('script')).toBe(script);

    unregisterThrowing();
    unregisterSecond();
  });

  it('replaces stale global dispatcher state when the version changes', () => {
    const globalObject = globalThis as Record<PropertyKey, unknown>;
    globalObject[dispatcherKey] = {
      handlers: new Map(),
      nextSequence: 0,
      orderedHandlers: [],
      version: 0,
    };

    const unregister = registerDomInsertionHandler({
      handle: () => true,
      id: 'alpha',
      priority: 100,
    });

    const state = globalObject[dispatcherKey] as {
      handlers: Map<number, unknown>;
      version: number;
    };

    expect(state.version).toBe(1);
    expect(state.handlers.size).toBe(1);
    expect(Element.prototype.appendChild).not.toBe(originalAppendChild);
    expect(Element.prototype.insertBefore).not.toBe(originalInsertBefore);

    unregister();
  });

  it('leaves no prototype residue across repeated install and reset cycles', () => {
    const guard = createScriptGuard({
      displayName: 'Alpha',
      id: 'alpha',
      isTargetUrl: (url) => url.includes('alpha.js'),
      proxyPath: '/integrations/alpha/sdk',
    });

    for (let attempt = 0; attempt < 3; attempt += 1) {
      guard.install();
      expect(Element.prototype.appendChild).not.toBe(originalAppendChild);
      expect(Element.prototype.insertBefore).not.toBe(originalInsertBefore);

      guard.reset();
      expect(Element.prototype.appendChild).toBe(originalAppendChild);
      expect(Element.prototype.insertBefore).toBe(originalInsertBefore);
    }
  });
});
