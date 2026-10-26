import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import {
  installDataDomeGuard,
  resetGuardState as resetDataDomeGuard,
} from '../../../src/integrations/datadome/script_guard';
import {
  installGtmGuard,
  resetGuardState as resetGtmGuard,
} from '../../../src/integrations/google_tag_manager/script_guard';
import {
  installGptGuard,
  resetGuardState as resetGptGuard,
} from '../../../src/integrations/gpt/script_guard';
import {
  installLockrGuard,
  resetGuardState as resetLockrGuard,
} from '../../../src/integrations/lockr/script_guard';
import {
  installPermutiveGuard,
  resetGuardState as resetPermutiveGuard,
} from '../../../src/integrations/permutive/script_guard';
import {
  installSourcepointGuard,
  resetGuardState as resetSourcepointGuard,
} from '../../../src/integrations/sourcepoint/script_guard';
import {
  type DomInsertionHandler,
  resetDomInsertionDispatcherForTests,
} from '../../../src/shared/dom_insertion_dispatcher';
import fixture from '../../fixtures/sdk-url-ownership.json';

interface UrlCase {
  name: string;
  url: string;
  owner: string | null;
  insertionOwner?: string;
}

const corpus: UrlCase[] = fixture;
const nativeSetAttribute = Element.prototype.setAttribute;

function resetGuards(): void {
  resetGptGuard();
  resetDataDomeGuard();
  resetGtmGuard();
  resetLockrGuard();
  resetPermutiveGuard();
  resetSourcepointGuard();
  resetDomInsertionDispatcherForTests();
}

function dispatcherState(): { orderedHandlers: DomInsertionHandler[] } {
  return (globalThis as Record<PropertyKey, unknown>)[
    Symbol.for('trusted-server.domInsertionDispatcher')
  ] as { orderedHandlers: DomInsertionHandler[] };
}

function expectedSrc({ url, owner, insertionOwner }: UrlCase): string {
  // jsdom's http: page resolves https: shorthand to an absolute vendor URL before insertion;
  // on an https: page the same input would resolve to a same-origin path and stay unowned.
  const actualOwner = insertionOwner ?? owner;
  if (!actualOwner) return url;
  const origin = window.location.origin;
  if (actualOwner === 'lockr' || actualOwner === 'permutive') {
    return `${origin}/integrations/${actualOwner}/sdk`;
  }
  const parsed = new URL(url, window.location.href);
  const prefix = actualOwner === 'sourcepoint' ? 'sourcepoint/cdn' : actualOwner;
  const fragment = actualOwner === 'gpt' ? parsed.hash : '';
  return `${origin}/integrations/${prefix}${parsed.pathname}${parsed.search}${fragment}`;
}

describe.each(['normal order', 'Lockr first'])('SDK URL ownership with %s', (order) => {
  beforeEach(() => {
    resetGuards();
    installPermutiveGuard();
    installLockrGuard();
    installSourcepointGuard();
    installGtmGuard();
    installDataDomeGuard();
    installGptGuard();

    if (order === 'Lockr first') {
      // Change real dispatch order, not installation order, only for this test.
      dispatcherState().orderedHandlers.sort(
        (left, right) => Number(right.id === 'lockr') - Number(left.id === 'lockr')
      );
    }
  });

  afterEach(resetGuards);

  it('installs all six guards', () => {
    expect(
      dispatcherState()
        .orderedHandlers.map(({ id }) => id)
        .sort()
    ).toEqual(['datadome', 'google_tag_manager', 'gpt', 'lockr', 'permutive', 'sourcepoint']);
  });

  it.each(corpus)('$name has only its expected raw claimant', ({ url, owner }) => {
    const claimants = dispatcherState()
      .orderedHandlers.filter((handler) => {
        // Do not set src: GPT's assignment patch would hide its raw ownership.
        const element = document.createElement('script');
        return handler.handle({ element, kind: 'script', url });
      })
      .map(({ id }) => id);

    expect(claimants).toEqual(owner ? [owner] : []);
  });

  describe.each(['script.src', 'native setAttribute'])('insertion through %s', (mode) => {
    it.each(corpus)('leaves $name to its owner', (row) => {
      const script = document.createElement('script');
      if (mode === 'script.src') {
        script.src = row.url;
      } else {
        nativeSetAttribute.call(script, 'src', row.url);
      }
      document.head.appendChild(script);
      expect(script.getAttribute('src')).toBe(expectedSrc(row));
      script.remove();
    });
  });

  describe.each(['preload', 'prefetch'])('%s script links', (rel) => {
    describe.each(['appendChild', 'insertBefore'])('through %s', (method) => {
      const affectedCases = corpus.filter(
        ({ owner }) => owner === null || owner === 'lockr' || owner === 'permutive'
      );

      it.each(affectedCases)('leaves $name to its owner', (row) => {
        const link = document.createElement('link');
        link.setAttribute('rel', rel);
        link.setAttribute('as', 'script');
        link.href = row.url;
        if (method === 'appendChild') {
          document.head.appendChild(link);
        } else {
          document.head.insertBefore(link, null);
        }
        expect(link.getAttribute('href')).toBe(expectedSrc(row));
        link.remove();
      });
    });
  });
});
