import { describe, expect, it } from 'vitest';

import { normalizeTrustedOrigin } from '../../src/shared/origin';

describe('shared/origin.ts', () => {
  it('accepts ordinary http(s) origins', () => {
    expect(normalizeTrustedOrigin('https://news.publisher.example')).toBe(
      'https://news.publisher.example'
    );
    expect(normalizeTrustedOrigin('http://localhost:7676')).toBe('http://localhost:7676');
  });

  it('accepts IPv6 literal origins', () => {
    // A DNS-shaped pattern rejects bracketed hosts, which would drop the stamp
    // and push the opaque-origin runtime onto the <base>-sensitive baseURI.
    expect(normalizeTrustedOrigin('http://[::1]:7676')).toBe('http://[::1]:7676');
    expect(normalizeTrustedOrigin('https://[2001:db8::1]')).toBe('https://[2001:db8::1]');
  });

  it('normalizes a full URL down to its origin', () => {
    expect(normalizeTrustedOrigin('https://publisher.example/some/page?q=1#frag')).toBe(
      'https://publisher.example'
    );
  });

  it('rejects opaque, non-http(s), and unparseable values', () => {
    expect(normalizeTrustedOrigin('null')).toBe('');
    expect(normalizeTrustedOrigin('about:srcdoc')).toBe('');
    expect(normalizeTrustedOrigin('javascript:alert(1)')).toBe('');
    expect(normalizeTrustedOrigin('data:text/html,x')).toBe('');
    expect(normalizeTrustedOrigin('/first-party/proxy')).toBe('');
    expect(normalizeTrustedOrigin('')).toBe('');
    expect(normalizeTrustedOrigin(undefined)).toBe('');
    expect(normalizeTrustedOrigin(42)).toBe('');
  });
});
