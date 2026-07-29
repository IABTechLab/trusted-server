import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { beforeEach, describe, expect, it, vi } from 'vitest';

const bootstrapPath = resolve(
  process.cwd(),
  '../../trusted-server-core/src/integrations/gpt_diagnostics_bootstrap.js'
);
const bootstrapSource = readFileSync(bootstrapPath, 'utf8');
const storageKey = 'tsjs:gptDiagnostics:active';

type BootstrapWindow = Window & {
  __tsjs_gpt_diagnostics_active?: boolean;
};

function runBootstrap(): void {
  window.eval(bootstrapSource);
}

function setUrl(url: string): void {
  window.history.replaceState({ fixture: true }, '', url);
}

function activeFlag(): boolean | undefined {
  return (window as BootstrapWindow).__tsjs_gpt_diagnostics_active;
}

describe('GPT diagnostics activation bootstrap', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    window.sessionStorage.clear();
    delete (window as BootstrapWindow).__tsjs_gpt_diagnostics_active;
    setUrl('/article?existing=1#section');
  });

  it.each(['1', 'true'])('activates the current tab for %s', (value) => {
    setUrl(`/article?existing=1&ts_console=${value}#section`);

    runBootstrap();

    expect(activeFlag()).toBe(true);
    expect(window.sessionStorage.getItem(storageKey)).toBe('1');
    expect(window.location.pathname).toBe('/article');
    expect(window.location.search).toBe('?existing=1');
    expect(window.location.hash).toBe('#section');
    expect(window.history.state).toEqual({ fixture: true });
  });

  it.each(['0', 'false'])('deactivates the current tab for %s', (value) => {
    window.sessionStorage.setItem(storageKey, '1');
    setUrl(`/article?ts_console=${value}&existing=1#section`);

    runBootstrap();

    expect(activeFlag()).toBe(false);
    expect(window.sessionStorage.getItem(storageKey)).toBe('0');
    expect(window.location.search).toBe('?existing=1');
    expect(window.location.hash).toBe('#section');
  });

  it('restores activation from session storage without a directive', () => {
    window.sessionStorage.setItem(storageKey, '1');

    runBootstrap();

    expect(activeFlag()).toBe(true);
    expect(window.location.search).toBe('?existing=1');
  });

  it('ignores case variants and leaves the directive visible', () => {
    window.sessionStorage.setItem(storageKey, '1');
    setUrl('/article?ts_console=True&existing=1#section');

    runBootstrap();

    expect(activeFlag()).toBe(true);
    expect(window.location.search).toBe('?ts_console=True&existing=1');
  });

  it('applies a recognized directive to the current document when storage throws', () => {
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('storage unavailable');
    });
    setUrl('/article?ts_console=true');

    expect(() => runBootstrap()).not.toThrow();
    expect(activeFlag()).toBe(true);
    expect(window.location.search).toBe('');
  });

  it('keeps activation when URL cleanup throws', () => {
    setUrl('/article?ts_console=true&existing=1#section');
    vi.spyOn(window.history, 'replaceState').mockImplementation(() => {
      throw new Error('history unavailable');
    });

    expect(() => runBootstrap()).not.toThrow();
    expect(activeFlag()).toBe(true);
    expect(window.sessionStorage.getItem(storageKey)).toBe('1');
    expect(window.location.search).toBe('?ts_console=true&existing=1');
  });

  it('removes every activation parameter after recognizing the first value', () => {
    setUrl('/article?ts_console=true&existing=1&ts_console=false#section');

    runBootstrap();

    expect(activeFlag()).toBe(true);
    expect(window.location.search).toBe('?existing=1');
  });

  it('cleans a recognized directive only once across repeated execution', () => {
    const nativeReplaceState = window.history.replaceState.bind(window.history);
    const replaceState = vi
      .spyOn(window.history, 'replaceState')
      .mockImplementation((data, unused, url) => nativeReplaceState(data, unused, url));
    setUrl('/article?ts_console=true&existing=1#section');
    replaceState.mockClear();

    runBootstrap();
    runBootstrap();

    expect(activeFlag()).toBe(true);
    expect(replaceState).toHaveBeenCalledTimes(1);
  });
});
