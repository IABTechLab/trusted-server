import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { writeEidsMirror } from '../../src/core/eids_mirror';

const NAME = '__Host-ts-eids-s';

describe('core/eids_mirror', () => {
  let writes: string[];
  let stored: string;

  beforeEach(() => {
    writes = [];
    stored = '';
    Object.defineProperty(document, 'cookie', {
      configurable: true,
      get: () => stored,
      set: (assignment: string) => {
        writes.push(assignment);
      },
    });
  });

  afterEach(() => {
    delete (document as unknown as { cookie?: string }).cookie;
    vi.restoreAllMocks();
  });

  it('writes the value with the given Max-Age and the __Host- attributes', () => {
    writeEidsMirror({ value: 'eyJ2IjoxfQ', max_age: 600 });

    expect(writes).toEqual([`${NAME}=eyJ2IjoxfQ; Path=/; Secure; SameSite=Lax; Max-Age=600`]);
  });

  it('skips the write when the cookie already holds that value', () => {
    stored = `other=1; ${NAME}=eyJ2IjoxfQ; ts-eids=abc`;

    writeEidsMirror({ value: 'eyJ2IjoxfQ', max_age: 600 });

    expect(writes).toEqual([]);
  });

  it('rewrites when the stored value differs', () => {
    stored = `${NAME}=old`;

    writeEidsMirror({ value: 'new', max_age: 60 });

    expect(writes).toHaveLength(1);
  });

  it('expires the cookie on clear', () => {
    writeEidsMirror({ clear: true });

    expect(writes).toEqual([`${NAME}=; Path=/; Secure; SameSite=Lax; Max-Age=0`]);
  });

  it.each([
    ['absent', undefined],
    ['null', null],
    ['a string', 'eyJ2IjoxfQ'],
    ['an empty object', {}],
    ['a clear that is not true', { clear: 'yes' }],
    ['a value without max_age', { value: 'abc' }],
    ['a non-numeric max_age', { value: 'abc', max_age: '60' }],
    ['a negative max_age', { value: 'abc', max_age: -1 }],
    ['a value with cookie separators', { value: 'a; Domain=example.com', max_age: 60 }],
    ['an empty value', { value: '', max_age: 60 }],
  ])('does nothing for %s', (_label, payload) => {
    writeEidsMirror(payload);

    expect(writes).toEqual([]);
  });

  it('never touches ts-eids', () => {
    stored = 'ts-eids=abc';

    writeEidsMirror({ value: 'abc', max_age: 60 });
    writeEidsMirror({ clear: true });

    expect(writes.every((write) => write.startsWith(`${NAME}=`))).toBe(true);
  });
});
