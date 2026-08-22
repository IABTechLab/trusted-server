import { describe, expect, it } from 'vitest';

import {
  normalizeTrustedOrigin,
  trustedDocumentHttpOrigin,
  trustedHttpOrigin,
} from '../../src/shared/origin';

describe('normalizeTrustedOrigin', () => {
  it('accepts ordinary and IPv6 HTTP(S) origins', () => {
    expect(normalizeTrustedOrigin('https://news.publisher.example')).toBe(
      'https://news.publisher.example'
    );
    expect(normalizeTrustedOrigin('http://localhost:7676')).toBe('http://localhost:7676');
    expect(normalizeTrustedOrigin('http://[::1]:7676')).toBe('http://[::1]:7676');
    expect(normalizeTrustedOrigin('https://[2001:db8::1]')).toBe('https://[2001:db8::1]');
  });

  it('normalizes a full URL down to its credential-free origin', () => {
    expect(normalizeTrustedOrigin('https://publisher.example/some/page?q=1#frag')).toBe(
      'https://publisher.example'
    );
    expect(normalizeTrustedOrigin('https://user:password@publisher.example/path')).toBe('');
  });

  it.each([
    'null',
    'about:srcdoc',
    'javascript:alert(1)',
    'data:text/html,x',
    '/first-party/proxy',
    '',
    undefined,
    42,
  ])('rejects an unusable candidate: %s', (candidate) => {
    expect(normalizeTrustedOrigin(candidate)).toBe('');
  });
});

describe('trustedHttpOrigin', () => {
  it('derives the exact publisher origin from a stamped or inherited base URL', () => {
    expect(trustedHttpOrigin('https://publisher.example')).toBe('https://publisher.example');
    expect(trustedHttpOrigin('http://publisher.example:8080/path/index.html')).toBe(
      'http://publisher.example:8080'
    );
  });

  it.each([
    '',
    'about:srcdoc',
    'data:text/html,creative',
    'javascript:alert(1)',
    'https://user:password@publisher.example/path',
  ])('fails closed for an unusable trusted base URL: %s', (candidate) => {
    expect(trustedHttpOrigin(candidate)).toBe('');
  });
});

describe('trustedDocumentHttpOrigin', () => {
  it('keeps a real document origin authoritative over a creative-only stamp', () => {
    expect(
      trustedDocumentHttpOrigin(
        'https://publisher.example',
        'https://publisher-script-spoof.example'
      )
    ).toBe('https://publisher.example');
  });

  it('uses the stamped base only for an opaque document origin', () => {
    expect(trustedDocumentHttpOrigin('null', 'https://publisher.example/article')).toBe(
      'https://publisher.example'
    );
  });
});
