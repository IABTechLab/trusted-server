import { describe, expect, it } from 'vitest';

import { trustedDocumentHttpOrigin, trustedHttpOrigin } from '../../src/shared/origin';

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
