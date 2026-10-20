import { afterEach, describe, expect, it } from 'vitest';

import { PUBLIC_PREFIX_ATTRIBUTE, cookieName, publicPrefix } from '../../src/shared/public_prefix';

function serveWith(prefix: string | null): void {
  document.getElementById('trustedserver-js')?.remove();
  const tag = document.createElement('script');
  tag.id = 'trustedserver-js';
  if (prefix !== null) tag.setAttribute(PUBLIC_PREFIX_ATTRIBUTE, prefix);
  document.head.appendChild(tag);
}

describe('public prefix', () => {
  afterEach(() => {
    document.getElementById('trustedserver-js')?.remove();
  });

  it('is read from the bundle tag the server writes it on', () => {
    serveWith('_ex.www.example.com');

    expect(publicPrefix()).toBe('_ex.www.example.com');
    expect(cookieName('eids')).toBe('_ex.www.example.com-eids');
  });

  it('names nothing on a page whose bundle tag carries none', () => {
    serveWith(null);
    expect(publicPrefix()).toBeNull();
    expect(cookieName('eids')).toBeNull();
  });

  it('refuses a value the server could never write', () => {
    for (const value of ['', '9ex', '__ex', '__Host', 'a b', 'a;b', 'a=b', 'a"b', 'a/b']) {
      serveWith(value);
      expect(publicPrefix(), value).toBeNull();
    }
  });
});
