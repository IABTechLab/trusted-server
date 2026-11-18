import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  isPubcidOptedOut,
  localStorageOptOutActive,
  syncPubcidOptOut,
} from '../../../src/integrations/prebid/pubcid_optout';

const NOW = Date.UTC(2026, 10, 18, 12, 0, 0);
const YEAR = 365 * 24 * 60 * 60;

function cookieValue(name: string): string | undefined {
  return document.cookie
    .split('; ')
    .find((part) => part.startsWith(`${name}=`))
    ?.slice(name.length + 1);
}

function clearAll(): void {
  for (const name of ['_pubcid_optout', '_ts_pubcid_optout_src']) {
    document.cookie = `${name}=; Path=/; Secure; Max-Age=0`;
  }
  localStorage.clear();
}

describe('localStorageOptOutActive', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW);
    clearAll();
  });
  afterEach(() => {
    vi.restoreAllMocks();
    clearAll();
    vi.useRealTimers();
  });

  it('matches Prebid for value and expiry combinations', () => {
    expect(localStorageOptOutActive(localStorage), 'should be inactive without a value').toBe(
      false
    );
    localStorage.setItem('_pubcid_optout', '');
    expect(localStorageOptOutActive(localStorage), 'should ignore an empty value').toBe(false);
    localStorage.setItem('_pubcid_optout', 'true');
    expect(localStorageOptOutActive(localStorage), 'should be active without expiry').toBe(true);
    localStorage.setItem('_pubcid_optout_exp', '');
    expect(localStorageOptOutActive(localStorage), 'should treat empty expiry as absent').toBe(
      true
    );
    localStorage.setItem('_pubcid_optout_exp', new Date(NOW + 60_000).toUTCString());
    expect(localStorageOptOutActive(localStorage), 'should honor a future expiry').toBe(true);
    localStorage.setItem('_pubcid_optout_exp', new Date(NOW - 60_000).toUTCString());
    expect(localStorageOptOutActive(localStorage), 'should drop a past expiry').toBe(false);
    localStorage.setItem('_pubcid_optout_exp', 'not a date');
    expect(localStorageOptOutActive(localStorage), 'should treat unparseable expiry as past').toBe(
      false
    );
  });
});

describe('syncPubcidOptOut', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW);
    clearAll();
  });
  afterEach(() => {
    vi.restoreAllMocks();
    clearAll();
    vi.useRealTimers();
  });

  it('copies an active localStorage opt-out with its marker for one year', () => {
    localStorage.setItem('_pubcid_optout', '1');
    const writes: string[] = [];
    const descriptor = Object.getOwnPropertyDescriptor(Document.prototype, 'cookie')!;
    vi.spyOn(document, 'cookie', 'set').mockImplementation((value: string) => {
      writes.push(value);
      descriptor.set!.call(document, value);
    });

    syncPubcidOptOut();

    expect(cookieValue('_pubcid_optout')).toBe('1');
    expect(cookieValue('_ts_pubcid_optout_src')).toBe('ls');
    expect(writes).toEqual([
      `_pubcid_optout=1; Path=/; Secure; SameSite=Lax; Max-Age=${YEAR}`,
      `_ts_pubcid_optout_src=ls; Path=/; Secure; SameSite=Lax; Max-Age=${YEAR}`,
    ]);
  });

  it('bounds the copy by a valid expiry', () => {
    localStorage.setItem('_pubcid_optout', '1');
    localStorage.setItem('_pubcid_optout_exp', new Date(NOW + 90_500).toUTCString());
    const writes: string[] = [];
    const descriptor = Object.getOwnPropertyDescriptor(Document.prototype, 'cookie')!;
    vi.spyOn(document, 'cookie', 'set').mockImplementation((value: string) => {
      writes.push(value);
      descriptor.set!.call(document, value);
    });

    syncPubcidOptOut();

    expect(writes[0], 'should expire with the localStorage opt-out').toMatch(/; Max-Age=90$/);
    expect(writes[0]).not.toContain(`Max-Age=${YEAR}`);
  });

  it('leaves a publisher cookie without a marker alone', () => {
    document.cookie = '_pubcid_optout=publisher; Path=/';
    localStorage.setItem('_pubcid_optout', '1');
    syncPubcidOptOut();
    expect(cookieValue('_pubcid_optout')).toBe('publisher');
    expect(cookieValue('_ts_pubcid_optout_src')).toBeUndefined();

    localStorage.clear();
    syncPubcidOptOut();
    expect(cookieValue('_pubcid_optout'), 'should keep the unmarked cookie').toBe('publisher');
  });

  it('expires its own copy once the localStorage opt-out ends', () => {
    localStorage.setItem('_pubcid_optout', '1');
    syncPubcidOptOut();
    localStorage.removeItem('_pubcid_optout');

    syncPubcidOptOut();

    expect(cookieValue('_pubcid_optout')).toBeUndefined();
    expect(cookieValue('_ts_pubcid_optout_src')).toBeUndefined();
  });

  it('does not rewrite an unchanged copy', () => {
    localStorage.setItem('_pubcid_optout', '1');
    syncPubcidOptOut();
    const set = vi.spyOn(document, 'cookie', 'set');
    syncPubcidOptOut();
    expect(set).not.toHaveBeenCalled();
  });

  it('changes nothing when storage throws', () => {
    localStorage.setItem('_pubcid_optout', '1');
    syncPubcidOptOut();
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('blocked');
    });

    expect(() => syncPubcidOptOut()).not.toThrow();

    expect(cookieValue('_pubcid_optout'), 'should keep the copy on storage errors').toBe('1');
    expect(cookieValue('_ts_pubcid_optout_src')).toBe('ls');
  });

  it('changes nothing when cookies throw', () => {
    localStorage.setItem('_pubcid_optout', '1');
    const writes: string[] = [];
    Object.defineProperty(document, 'cookie', {
      configurable: true,
      get: () => {
        throw new Error('blocked');
      },
      set: (value: string) => writes.push(value),
    });
    try {
      expect(() => syncPubcidOptOut()).not.toThrow();
    } finally {
      delete (document as unknown as Record<string, unknown>).cookie;
    }
    expect(writes).toEqual([]);
  });
});

describe('isPubcidOptedOut', () => {
  beforeEach(clearAll);
  afterEach(() => {
    vi.restoreAllMocks();
    clearAll();
  });

  it('reports a cookie-only or localStorage-only opt-out', () => {
    expect(isPubcidOptedOut()).toBe(false);
    document.cookie = '_pubcid_optout=; Path=/';
    expect(isPubcidOptedOut(), 'should ignore an empty cookie as Prebid does').toBe(false);
    document.cookie = '_pubcid_optout=yes; Path=/';
    expect(isPubcidOptedOut(), 'should honor a cookie opt-out').toBe(true);
    document.cookie = '_pubcid_optout=; Path=/; Max-Age=0';
    localStorage.setItem('_pubcid_optout', '1');
    expect(isPubcidOptedOut(), 'should honor a localStorage opt-out').toBe(true);
  });

  it('fails closed when storage cannot be read', () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('blocked');
    });
    expect(isPubcidOptedOut()).toBe(true);
  });
});
