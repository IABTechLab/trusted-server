import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  installLockrApiGuard,
  resetApiGuardState,
} from '../../../src/integrations/lockr/api_guard';

describe('Lockr API origin guard', () => {
  let base: HTMLBaseElement;

  beforeEach(() => {
    resetApiGuardState();
    base = document.createElement('base');
    base.href = 'https://external-base.example/assets/';
    document.head.appendChild(base);
  });

  afterEach(() => {
    resetApiGuardState();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    base.remove();
  });

  it.each([
    '/integrations/lockr/api',
    '/integrations/lockr/api?key=value',
    '/integrations/lockr/api#fragment',
    '/integrations/lockr/api/publisher/app/v2/identityLockr/settings',
    '/integrations/lockr/api/publisher/app/v2/identityLockr/page-view?key=value#fragment',
    '/integrations/lockr/api/publisher/app/v2/identityLockr/revoke-consent',
  ])('pins %s to the publisher origin', (url) => {
    const open = vi.spyOn(XMLHttpRequest.prototype, 'open');
    installLockrApiGuard();

    new XMLHttpRequest().open('POST', url);

    expect(open).toHaveBeenCalledExactlyOnceWith('POST', `${window.location.origin}${url}`);
  });

  it.each([
    '/integrations/lockr/apiary',
    '/integrations/lockr/api-v2/settings',
    '/integrations/other/api/settings',
    'integrations/lockr/api/settings',
    '//vendor.example/integrations/lockr/api/settings',
    'https://vendor.example/integrations/lockr/api/settings',
    'https://publisher.example/integrations/lockr/api/settings',
  ])('does not change unrelated or explicitly addressed URL %s', (url) => {
    const open = vi.spyOn(XMLHttpRequest.prototype, 'open');
    installLockrApiGuard();

    new XMLHttpRequest().open('GET', url);

    expect(open).toHaveBeenCalledExactlyOnceWith('GET', url);
  });

  it('preserves URL objects', () => {
    const open = vi.spyOn(XMLHttpRequest.prototype, 'open');
    const url = new URL('/integrations/lockr/api/settings', window.location.origin);
    installLockrApiGuard();

    new XMLHttpRequest().open('POST', url);

    expect(open).toHaveBeenCalledExactlyOnceWith('POST', url);
  });

  it('preserves the receiver, sync mode, and authentication arguments', () => {
    const open = vi.spyOn(XMLHttpRequest.prototype, 'open');
    installLockrApiGuard();
    const request = new XMLHttpRequest();

    request.open('POST', '/integrations/lockr/api/settings', false, 'user', 'password');

    expect(open).toHaveBeenCalledExactlyOnceWith(
      'POST',
      `${window.location.origin}/integrations/lockr/api/settings`,
      false,
      'user',
      'password'
    );
    expect(open.mock.contexts[0]).toBe(request);
  });

  it('preserves explicitly undefined optional arguments', () => {
    const open = vi.spyOn(XMLHttpRequest.prototype, 'open');
    installLockrApiGuard();

    new XMLHttpRequest().open('GET', '/integrations/lockr/api/settings', true, undefined, null);

    expect(open).toHaveBeenCalledExactlyOnceWith(
      'GET',
      `${window.location.origin}/integrations/lockr/api/settings`,
      true,
      undefined,
      null
    );
  });

  it('preserves native argument validation', () => {
    installLockrApiGuard();
    const request = new XMLHttpRequest();

    expect(() => Reflect.apply(request.open, request, [])).toThrow('2 arguments required');
    expect(() => request.open('INVALID METHOD', '/integrations/lockr/api/settings')).toThrow();
  });

  it('handles an external base added after installation without polling', () => {
    base.remove();
    const open = vi.spyOn(XMLHttpRequest.prototype, 'open');
    vi.useFakeTimers();

    try {
      installLockrApiGuard();
      document.head.appendChild(base);
      vi.advanceTimersByTime(10_000);

      new XMLHttpRequest().open('POST', '/integrations/lockr/api/settings');

      expect(open).toHaveBeenCalledExactlyOnceWith(
        'POST',
        `${window.location.origin}/integrations/lockr/api/settings`
      );
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it('installs only once and restores the previous method on reset', () => {
    const originalOpen = XMLHttpRequest.prototype.open;
    installLockrApiGuard();
    const guardedOpen = XMLHttpRequest.prototype.open;

    installLockrApiGuard();

    expect(guardedOpen).not.toBe(originalOpen);
    expect(XMLHttpRequest.prototype.open).toBe(guardedOpen);

    resetApiGuardState();
    resetApiGuardState();

    expect(XMLHttpRequest.prototype.open).toBe(originalOpen);

    installLockrApiGuard();
    expect(XMLHttpRequest.prototype.open).not.toBe(originalOpen);
  });

  it('does not overwrite a later XHR interceptor during reset', () => {
    const originalOpen = XMLHttpRequest.prototype.open;
    installLockrApiGuard();
    const laterOpen = vi.fn(originalOpen);
    XMLHttpRequest.prototype.open = laterOpen;

    try {
      resetApiGuardState();
      expect(XMLHttpRequest.prototype.open).toBe(laterOpen);
    } finally {
      XMLHttpRequest.prototype.open = originalOpen;
    }
  });

  it('does not patch XHR outside a browser', () => {
    const originalOpen = XMLHttpRequest.prototype.open;
    vi.stubGlobal('window', undefined);

    installLockrApiGuard();

    expect(XMLHttpRequest.prototype.open).toBe(originalOpen);
  });

  it('does not require XHR to be available', () => {
    vi.stubGlobal('XMLHttpRequest', undefined);

    expect(() => installLockrApiGuard()).not.toThrow();
  });
});
