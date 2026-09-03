import { describe, it, expect, beforeEach, vi } from 'vitest';

import type { PermissionsSnapshot, TsjsApi } from '../../src/core/types';

describe('core/permissions', () => {
  // The bundle is injected at head open, so the document is still parsing
  // when core initializes. Tests set the state explicitly because the
  // fallback path depends on it.
  function setReadyState(state: DocumentReadyState): void {
    Object.defineProperty(document, 'readyState', {
      value: state,
      configurable: true,
    });
  }

  beforeEach(async () => {
    await vi.resetModules();
    document.body.innerHTML = '';
    delete window.tsjs;
    setReadyState('loading');
  });

  it('keeps permissions injected before the bundle loads and resolves with them', async () => {
    const injected: PermissionsSnapshot = { set: ['necessary.operations'] };
    window.tsjs = { permissions: injected } as TsjsApi;

    await import('../../src/core/index');
    const api = window.tsjs as TsjsApi;

    expect(api.permissions).toEqual({ set: ['necessary.operations'] });
    await expect(api.whenPermissions!()).resolves.toEqual({ set: ['necessary.operations'] });
  });

  it('resolves on the body seam assignment and reads the value back', async () => {
    await import('../../src/core/index');
    const api = window.tsjs as TsjsApi;

    const settled = api.whenPermissions!();
    api.permissions = { set: ['marketing.advertising.serving'] };

    await expect(settled).resolves.toEqual({ set: ['marketing.advertising.serving'] });
    expect(api.permissions).toEqual({ set: ['marketing.advertising.serving'] });
  });

  it('falls back to the empty default when no assignment arrives before DOMContentLoaded', async () => {
    await import('../../src/core/index');
    const api = window.tsjs as TsjsApi;

    const settled = api.whenPermissions!();
    document.dispatchEvent(new Event('DOMContentLoaded'));

    await expect(settled).resolves.toEqual({ set: [] });
  });

  it('resolves at once when the document has already been parsed', async () => {
    // A bundle initializing after parsing has missed any seam, so waiting for
    // DOMContentLoaded would wait forever.
    setReadyState('interactive');
    await import('../../src/core/index');
    const api = window.tsjs as TsjsApi;

    await expect(api.whenPermissions!()).resolves.toEqual({ set: [] });
  });

  it('returns the same resolved value from every later call', async () => {
    await import('../../src/core/index');
    const api = window.tsjs as TsjsApi;

    api.permissions = { set: ['analytics.reporting'] };
    const first = await api.whenPermissions!();
    const second = await api.whenPermissions!();

    expect(second).toBe(first);
    expect(second).toEqual({ set: ['analytics.reporting'] });
  });
});
