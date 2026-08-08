import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { describe, expect, it, vi } from 'vitest';

const bootstrapPath = resolve(
  process.cwd(),
  '../../trusted-server-core/src/integrations/gpt_diagnostics_bootstrap.js'
);
const bootstrapSource = readFileSync(bootstrapPath, 'utf8');
describe('GPT diagnostics activation ownership', () => {
  it('only performs one server-authorized raw URL cleanup without publishing authority', () => {
    const replaceState = vi.fn();
    const state = Object.freeze({ publisher: 'state' });
    const location = Object.freeze({
      href: 'https://publisher.example/a%2Fb?keep=%2F&ts_console=1&space=a+b&ts_console=bogus#frag%20x',
    });
    const history = Object.freeze({ replaceState, state });

    Function('location', 'history', bootstrapSource)(location, history);

    expect(replaceState).toHaveBeenCalledExactlyOnceWith(
      state,
      '',
      'https://publisher.example/a%2Fb?keep=%2F&space=a+b#frag%20x'
    );
    expect(bootstrapSource).not.toMatch(/sessionStorage|localStorage/);
    expect(bootstrapSource).not.toMatch(/__tsjs_gpt_diagnostics_active/);
    expect(bootstrapSource).not.toMatch(/window\s*\[/);
  });

  it('contains replaceState failure and preserves an unrelated URL without a call', () => {
    const replaceState = vi.fn(() => {
      throw new Error('fictional history failure');
    });
    expect(() =>
      Function('location', 'history', bootstrapSource)(
        Object.freeze({ href: 'https://publisher.example/?ts_console=false#kept' }),
        Object.freeze({ replaceState, state: null })
      )
    ).not.toThrow();
    expect(replaceState).toHaveBeenCalledOnce();

    replaceState.mockClear();
    Function('location', 'history', bootstrapSource)(
      Object.freeze({ href: 'https://publisher.example/?contest_console=1#kept' }),
      Object.freeze({ replaceState, state: null })
    );
    expect(replaceState).not.toHaveBeenCalled();
  });

  it.each([
    ['https://publisher.example/a?ts_console=1', 'https://publisher.example/a'],
    ['https://publisher.example/a?ts_console=1&', 'https://publisher.example/a'],
    [
      'https://publisher.example/a?&ts_console=1&keep=%2F',
      'https://publisher.example/a?&keep=%2F',
    ],
  ])('matches the server sanitizer for empty raw query segments', (href, expected) => {
    const replaceState = vi.fn();

    Function('location', 'history', bootstrapSource)(
      Object.freeze({ href }),
      Object.freeze({ replaceState, state: null })
    );

    expect(replaceState).toHaveBeenCalledExactlyOnceWith(null, '', expected);
  });
});
