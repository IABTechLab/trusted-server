import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  createBrowserNavigationIdentityIssuer,
  createTestNavigationIdentityIssuer,
  mintTestLifecycleTicket,
  mintTestRendererNonce,
  type RandomValuesSource,
} from '../../src/kernel/identity';

function decodeIdentity(value: string): Buffer {
  return Buffer.from(value.slice(3), 'base64url');
}

function deterministicSource(bytes: readonly number[]): {
  readonly source: RandomValuesSource;
  readonly calls: ReturnType<typeof vi.fn>;
} {
  let offset = 0;
  const calls = vi.fn((target: Uint8Array<ArrayBuffer>): Uint8Array<ArrayBuffer> => {
    for (let index = 0; index < target.length; index += 1) {
      target[index] = bytes[offset % bytes.length] ?? 0;
      offset += 1;
    }
    return target;
  });
  return { source: calls, calls };
}

describe('navigation identity issuer', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('draws one eight-byte prefix and increments a big-endian u64 ordinal once per attempt', () => {
    const { source, calls } = deterministicSource([0, 1, 2, 3, 4, 5, 6, 7]);
    const created = createTestNavigationIdentityIssuer({ getRandomValues: source });

    expect(created).toMatchObject({ ok: true });
    if (!created.ok) throw new Error('Expected an identity issuer');

    const first = created.value.mintAttemptId();
    const second = created.value.mintAttemptId();

    expect(first).toEqual({ ok: true, value: 'a1_AAECAwQFBgcAAAAAAAAAAQ' });
    expect(second).toEqual({ ok: true, value: 'a1_AAECAwQFBgcAAAAAAAAAAg' });
    expect(first.ok && decodeIdentity(first.value)).toEqual(
      Buffer.from([0, 1, 2, 3, 4, 5, 6, 7, 0, 0, 0, 0, 0, 0, 0, 1])
    );
    expect(second.ok && decodeIdentity(second.value)).toEqual(
      Buffer.from([0, 1, 2, 3, 4, 5, 6, 7, 0, 0, 0, 0, 0, 0, 0, 2])
    );
    expect(first.ok && first.value).toHaveLength(25);
    expect(second.ok && second.value).toHaveLength(25);
    expect(calls).toHaveBeenCalledOnce();
    expect(calls.mock.calls[0]?.[0]).toHaveLength(8);
    expect(created.value.snapshotOrdinalForTest()).toEqual([0, 2]);
  });

  it('issues the final ordinal once and then fails forever without wrapping', () => {
    const { source } = deterministicSource([8, 7, 6, 5, 4, 3, 2, 1]);
    const failure = vi.fn();
    const created = createTestNavigationIdentityIssuer({
      getRandomValues: source,
      initialOrdinal: [0xffff_ffff, 0xffff_fffe],
      onFailure: failure,
    });

    expect(created).toMatchObject({ ok: true });
    if (!created.ok) throw new Error('Expected an identity issuer');

    expect(created.value.mintAttemptId()).toMatchObject({ ok: true });
    expect(created.value.snapshotOrdinalForTest()).toEqual([0xffff_ffff, 0xffff_ffff]);
    expect(created.value.mintAttemptId()).toEqual({
      ok: false,
      reason: 'identity_generation_failed',
    });
    expect(created.value.mintAttemptId()).toEqual({
      ok: false,
      reason: 'identity_generation_failed',
    });
    expect(created.value.snapshotOrdinalForTest()).toEqual([0xffff_ffff, 0xffff_ffff]);
    expect(failure).toHaveBeenCalledTimes(2);
    expect(failure.mock.calls).toEqual([
      ['identity_generation_failed'],
      ['identity_generation_failed'],
    ]);
  });

  it('fails before creating an issuer when browser crypto is missing or throws', () => {
    vi.stubGlobal('crypto', undefined);
    expect(createBrowserNavigationIdentityIssuer()).toEqual({
      ok: false,
      reason: 'identity_generation_failed',
    });

    vi.stubGlobal('crypto', {
      getRandomValues: () => {
        throw new Error('unavailable');
      },
    });
    expect(createBrowserNavigationIdentityIssuer()).toEqual({
      ok: false,
      reason: 'identity_generation_failed',
    });
  });

  it('reports prefix failures without exposing raw bytes or identities', () => {
    const failure = vi.fn();
    const created = createTestNavigationIdentityIssuer({
      getRandomValues: () => {
        throw new Error('sensitive source failure');
      },
      onFailure: failure,
    });

    expect(created).toEqual({ ok: false, reason: 'identity_generation_failed' });
    expect(failure.mock.calls).toEqual([['identity_generation_failed']]);
  });
});

describe('fresh capability identities', () => {
  it('encodes each lifecycle ticket from sixteen fresh CSPRNG bytes', () => {
    const { source, calls } = deterministicSource([
      0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
    ]);

    const first = mintTestLifecycleTicket(source);
    const second = mintTestLifecycleTicket(source);

    expect(first).toEqual({ ok: true, value: 't1_AAECAwQFBgcICQoLDA0ODw' });
    expect(second).toEqual({ ok: true, value: 't1_AAECAwQFBgcICQoLDA0ODw' });
    expect(first.ok && first.value).toHaveLength(25);
    expect(first.ok && decodeIdentity(first.value)).toHaveLength(16);
    expect(calls).toHaveBeenCalledTimes(2);
    expect(calls.mock.calls[0]?.[0]).not.toBe(calls.mock.calls[1]?.[0]);
  });

  it('encodes each renderer nonce from sixteen fresh CSPRNG bytes', () => {
    const { source, calls } = deterministicSource([
      15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0,
    ]);

    const result = mintTestRendererNonce(source);

    expect(result).toEqual({ ok: true, value: 'n1_Dw4NDAsKCQgHBgUEAwIBAA' });
    expect(result.ok && result.value).toHaveLength(25);
    expect(result.ok && decodeIdentity(result.value)).toHaveLength(16);
    expect(calls).toHaveBeenCalledOnce();
    expect(calls.mock.calls[0]?.[0]).toHaveLength(16);
  });

  it('maps ticket and nonce source failures without leaking source values', () => {
    const failure = vi.fn();
    const source = () => {
      throw new Error('sensitive source failure');
    };

    expect(mintTestLifecycleTicket(source, failure)).toEqual({
      ok: false,
      reason: 'identity_generation_failed',
    });
    expect(mintTestRendererNonce(source, failure)).toEqual({
      ok: false,
      reason: 'identity_generation_failed',
    });
    expect(failure.mock.calls).toEqual([
      ['identity_generation_failed'],
      ['identity_generation_failed'],
    ]);
  });
});
