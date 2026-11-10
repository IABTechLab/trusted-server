import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import {
  installLockrGuard,
  resetGuardState as resetLockrGuard,
} from '../../src/integrations/lockr/script_guard';
import {
  installPermutiveGuard,
  resetGuardState as resetPermutiveGuard,
} from '../../src/integrations/permutive/script_guard';
import { parseSdkNetworkUrl } from '../../src/shared/sdk_url';

describe.each(['http:', 'https:'])('SDK shorthand with an %s document base', (baseScheme) => {
  let base: HTMLBaseElement;
  let container: HTMLDivElement;

  beforeEach(() => {
    base = document.createElement('base');
    base.href = `${baseScheme}//publisher.example.com/page`;
    document.head.appendChild(base);
    container = document.createElement('div');
    document.body.appendChild(container);
    installLockrGuard();
    installPermutiveGuard();
  });

  afterEach(() => {
    resetLockrGuard();
    resetPermutiveGuard();
    container.remove();
    base.remove();
  });

  it.each([
    '\0https://aim.loc.kr/identity-lockr.js',
    'https://aim.loc.kr/identity-lockr.js\0',
    '\0https://cdn.permutive.com/app-web.js',
    'https://cdn.permutive.com/app-web.js\0',
  ])('leaves NUL-containing static markup unowned: %s', (raw) => {
    expect(parseSdkNetworkUrl(raw)).toBeNull();
    const template = document.createElement('template');
    template.innerHTML = `<script src="${raw}"></script>`;
    const script = template.content.querySelector('script')!;
    const attribute = raw.split('\0').join('\ufffd');
    expect(script.getAttribute('src')).toBe(attribute);

    container.appendChild(script);
    expect(script.getAttribute('src')).toBe(attribute);
  });

  it.each([
    ['http:', 'aim.loc.kr/identity-lockr.js', 'lockr'],
    ['https:', 'aim.loc.kr/identity-lockr.js', 'lockr'],
    ['http:', 'cdn.permutive.com/app-web.js', 'permutive'],
    ['https:', 'cdn.permutive.com/app-web.js', 'permutive'],
  ])('resolves %s%s before the %s guard claims it', (scheme, path, owner) => {
    const raw = `${scheme}${path}`;
    expect(parseSdkNetworkUrl(raw)).toBeNull();

    const script = document.createElement('script');
    script.setAttribute('src', raw);
    expect(script.src).toBe(
      scheme === baseScheme ? `${baseScheme}//publisher.example.com/${path}` : `${scheme}//${path}`
    );
    container.appendChild(script);

    expect(script.getAttribute('src')).toBe(
      scheme === baseScheme ? raw : `${window.location.origin}/integrations/${owner}/sdk`
    );
  });
});
