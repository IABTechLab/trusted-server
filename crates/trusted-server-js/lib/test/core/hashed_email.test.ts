import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import {
  clearHashedEmail,
  normalizeEmail,
  setEmail,
  setHashedEmail,
} from '../../src/core/hashed_email';

const EXAMPLE_HASH = 'ab'.repeat(32);

describe('core/hashed_email', () => {
  let writes: string[];

  beforeEach(() => {
    writes = [];
    Object.defineProperty(document, 'cookie', {
      configurable: true,
      get: () => '',
      set: (assignment: string) => {
        writes.push(assignment);
      },
    });
  });

  afterEach(() => {
    delete (document as unknown as { cookie?: string }).cookie;
  });

  it('normalizes case, whitespace and Gmail aliases', () => {
    expect(normalizeEmail('  Person@Example.COM ')).toBe('person@example.com');
    expect(normalizeEmail('first.last+news@gmail.com')).toBe('firstlast@gmail.com');
    expect(normalizeEmail('first.last+news@example.com')).toBe('first.last+news@example.com');
    expect(normalizeEmail('not-an-address')).toBeUndefined();
    expect(normalizeEmail('@example.com')).toBeUndefined();
  });

  it('stores a hex digest in ts-hem', () => {
    expect(setHashedEmail(EXAMPLE_HASH.toUpperCase())).toBe(true);
    expect(writes).toEqual([
      `ts-hem=${EXAMPLE_HASH}; Path=/; Secure; SameSite=Lax; Max-Age=2592000`,
    ]);
  });

  it('refuses a value that is not a digest', () => {
    expect(setHashedEmail('person@example.com')).toBe(false);
    expect(writes).toEqual([]);
  });

  it('hashes the normalized email without storing the address', async () => {
    const stored = await setEmail(' Person@Example.com ');

    const expected = Array.from(
      new Uint8Array(
        await crypto.subtle.digest('SHA-256', new TextEncoder().encode('person@example.com'))
      ),
      (byte) => byte.toString(16).padStart(2, '0')
    ).join('');
    expect(stored).toBe(true);
    expect(writes).toEqual([`ts-hem=${expected}; Path=/; Secure; SameSite=Lax; Max-Age=2592000`]);
    expect(writes[0]).not.toContain('example.com');
  });

  it('matches the published SHA-256 vector for a normalized address', async () => {
    await setEmail('Test@Example.com');

    expect(writes).toEqual([
      'ts-hem=973dfe463ec85785f5f95af5ba3906eedb2d931c24e69824a89ea65dba4e813b; Path=/; Secure; SameSite=Lax; Max-Age=2592000',
    ]);
  });

  it.each([
    ['googlemail aliases', 'J.Doe+promo@GoogleMail.com', 'jdoe@googlemail.com'],
    ['a dotted Gmail local part', 'j.d.o.e@gmail.com', 'jdoe@gmail.com'],
    ['several plus signs on Gmail', 'jdoe+a+b@gmail.com', 'jdoe@gmail.com'],
    ['dots on another domain', 'j.doe@example.com', 'j.doe@example.com'],
  ])('normalizes %s', (_label, input, expected) => {
    expect(normalizeEmail(input)).toBe(expected);
  });

  it.each([
    ['an empty string', ''],
    ['only whitespace', '   '],
    ['a missing domain', 'person@'],
    ['inner whitespace', 'per son@example.com'],
    ['two at signs', 'a@b@example.com'],
    ['a Gmail local part that is only a suffix', '+news@gmail.com'],
    ['a Gmail local part that is only dots', '..@gmail.com'],
  ])('refuses %s', (_label, input) => {
    expect(normalizeEmail(input)).toBeUndefined();
  });

  it('writes nothing for an address it refuses', async () => {
    expect(await setEmail('not-an-address')).toBe(false);
    expect(writes).toEqual([]);
  });

  it('writes nothing when Web Crypto is unavailable', async () => {
    const original = Object.getOwnPropertyDescriptor(globalThis, 'crypto');
    Object.defineProperty(globalThis, 'crypto', { configurable: true, value: {} });
    try {
      expect(await setEmail('person@example.com')).toBe(false);
    } finally {
      if (original) {
        Object.defineProperty(globalThis, 'crypto', original);
      }
    }
    expect(writes).toEqual([]);
  });

  it.each([
    ['63 digits', 'a'.repeat(63)],
    ['65 digits', 'a'.repeat(65)],
    ['a non-hex digit', 'g'.repeat(64)],
    ['a cookie separator', `${'a'.repeat(63)};`],
  ])('refuses a digest with %s', (_label, value) => {
    expect(setHashedEmail(value)).toBe(false);
    expect(writes).toEqual([]);
  });

  it('trims a digest before storing it', () => {
    expect(setHashedEmail(`  ${EXAMPLE_HASH}\n`)).toBe(true);
    expect(writes[0]).toBe(`ts-hem=${EXAMPLE_HASH}; Path=/; Secure; SameSite=Lax; Max-Age=2592000`);
  });

  it('does not throw when the cookie cannot be written', () => {
    Object.defineProperty(document, 'cookie', {
      configurable: true,
      get: () => '',
      set: () => {
        throw new Error('sandboxed');
      },
    });

    expect(() => setHashedEmail(EXAMPLE_HASH)).not.toThrow();
    expect(() => clearHashedEmail()).not.toThrow();
  });

  it('clears ts-hem', () => {
    clearHashedEmail();
    expect(writes).toEqual(['ts-hem=; Path=/; Secure; SameSite=Lax; Max-Age=0']);
  });
});
