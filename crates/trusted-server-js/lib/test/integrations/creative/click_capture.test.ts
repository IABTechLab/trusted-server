import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { FIRST_PARTY_CLICK, MUTATED_CLICK, importCreativeModule } from './helpers';

const originalFetch = global.fetch;
const currentScriptDescriptor = Object.getOwnPropertyDescriptor(document, 'currentScript');

beforeEach(() => {
  vi.resetModules();
  document.body.innerHTML = '';
  vi.useFakeTimers();
});

afterEach(() => {
  global.fetch = originalFetch;
  vi.useRealTimers();
  if (currentScriptDescriptor) {
    Object.defineProperty(document, 'currentScript', currentScriptDescriptor);
  } else {
    Reflect.deleteProperty(document, 'currentScript');
  }
});

describe('creative/click.ts captured first-party fallback', () => {
  it('installs an absolute GET fallback when fetch is unavailable', async () => {
    const script = Object.assign(document.createElement('script'), {
      src: 'https://ads.example.com:8443/static/tsjs=tsjs-unified.min.js?v=hash',
    });
    Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
    global.fetch = undefined as unknown as typeof fetch;

    const anchor = document.createElement('a');
    anchor.setAttribute('data-tsclick', FIRST_PARTY_CLICK);
    anchor.setAttribute('href', FIRST_PARTY_CLICK);
    document.body.appendChild(anchor);

    await importCreativeModule();
    anchor.setAttribute('href', MUTATED_CLICK);
    await Promise.resolve();
    await vi.runAllTimersAsync();

    const fallback = anchor.getAttribute('href') ?? '';
    expect(fallback).toMatch(/^https:\/\/ads\.example\.com:8443\/first-party\/proxy-rebuild\?/);
    expect(anchor.getAttribute('data-tsclick')).toBe(fallback);
  });
});
