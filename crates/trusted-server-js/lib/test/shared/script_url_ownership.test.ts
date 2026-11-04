import { afterEach, describe, expect, it } from 'vitest';

import {
  installDataDomeGuard,
  resetGuardState as resetDataDome,
} from '../../src/integrations/datadome/script_guard';
import {
  installGtmGuard,
  resetGuardState as resetGtm,
} from '../../src/integrations/google_tag_manager/script_guard';
import {
  installGptGuard,
  resetGuardState as resetGpt,
} from '../../src/integrations/gpt/script_guard';
import {
  installLockrGuard,
  resetGuardState as resetLockr,
} from '../../src/integrations/lockr/script_guard';
import {
  installPermutiveGuard,
  resetGuardState as resetPermutive,
} from '../../src/integrations/permutive/script_guard';
import {
  installSourcepointGuard,
  resetGuardState as resetSourcepoint,
} from '../../src/integrations/sourcepoint/script_guard';
import cases from '../fixtures/script-url-ownership.json';

const guards = [
  { id: 'lockr', install: installLockrGuard, reset: resetLockr },
  { id: 'permutive', install: installPermutiveGuard, reset: resetPermutive },
  { id: 'datadome', install: installDataDomeGuard, reset: resetDataDome },
  { id: 'google_tag_manager', install: installGtmGuard, reset: resetGtm },
  { id: 'gpt', install: installGptGuard, reset: resetGpt },
  { id: 'sourcepoint', install: installSourcepointGuard, reset: resetSourcepoint },
];

function resetGuards(): void {
  for (const guard of guards) guard.reset();
  document.head.innerHTML = '';
}

afterEach(resetGuards);

function insert(url: string, kind: string): { original: string; actual: string } {
  const container = document.createElement('div');
  let original = url;
  try {
    original = new URL(url, document.baseURI).href;
  } catch {
    // A malformed reference remains the raw DOM attribute value.
  }
  if (kind === 'script') {
    const script = document.createElement('script');
    script.setAttribute('src', url);
    container.appendChild(script);
    return { original, actual: script.src };
  }
  const link = document.createElement('link');
  link.setAttribute('rel', kind);
  link.setAttribute('as', 'script');
  link.setAttribute('href', url);
  container.insertBefore(link, null);
  return { original, actual: link.href };
}

function setBase(base: string): void {
  const element = document.createElement('base');
  element.href = base;
  document.head.appendChild(element);
}

describe('built-in script URL ownership corpus', () => {
  for (const entry of cases) {
    for (const kind of ['script', 'preload', 'prefetch']) {
      it(`${entry.id}: ${kind} has only its parsed URL owner`, () => {
        const claimants: string[] = [];
        for (const guard of guards) {
          resetGuards();
          setBase(entry.base ?? 'https://edge.example.com/news/');
          guard.install();
          const result = insert(entry.url, kind);
          if (result.actual !== result.original) claimants.push(guard.id);
          if (guard.id === entry.dom) {
            expect(result.actual).toBe(`${window.location.origin}${entry.domPath}`);
          } else {
            expect(result.actual).toBe(result.original);
          }
        }
        expect(claimants).toEqual(entry.dom ? [entry.dom] : []);
      });
    }
  }

  for (const reverse of [false, true]) {
    it(`preserves ownership with all guards installed, reverse=${reverse}`, () => {
      setBase('https://edge.example.com/news/');
      for (const guard of reverse ? [...guards].reverse() : guards) guard.install();
      for (const entry of cases.filter((entry) => entry.dom && !entry.base)) {
        const result = insert(entry.url, 'script');
        expect(result.actual, entry.id).toBe(`${window.location.origin}${entry.domPath}`);
      }
    });
  }
});
