import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import {
  installSourcepointGuard,
  isGuardInstalled,
  isSourcepointUrl,
  resetGuardState,
  rewriteSourcepointUrl,
} from '../../../src/integrations/sourcepoint/script_guard';

describe('Sourcepoint SDK Script Interception Guard', () => {
  let originalAppendChild: typeof Element.prototype.appendChild;
  let originalInsertBefore: typeof Element.prototype.insertBefore;

  beforeEach(() => {
    resetGuardState();
    originalAppendChild = Element.prototype.appendChild;
    originalInsertBefore = Element.prototype.insertBefore;
  });

  afterEach(() => {
    resetGuardState();
  });

  it('detects Sourcepoint CDN URLs', () => {
    expect(isSourcepointUrl('https://cdn.privacy-mgmt.com/wrapper/v2/messages')).toBe(true);
    expect(isSourcepointUrl('//cdn.privacy-mgmt.com/mms/v2/get_site_data')).toBe(true);
    expect(isSourcepointUrl('https://example.com/script.js')).toBe(false);
    expect(isSourcepointUrl('https://geo.privacymanager.io/')).toBe(false);
  });

  it('rewrites CDN URLs to the first-party proxy path', () => {
    expect(
      rewriteSourcepointUrl('https://cdn.privacy-mgmt.com/wrapper/v2/messages?env=prod')
    ).toBe(
      `${window.location.origin}/integrations/sourcepoint/cdn/wrapper/v2/messages?env=prod`
    );
  });

  it('installs and resets the guard', () => {
    expect(isGuardInstalled()).toBe(false);
    installSourcepointGuard();
    expect(isGuardInstalled()).toBe(true);
    expect(Element.prototype.appendChild).not.toBe(originalAppendChild);
    expect(Element.prototype.insertBefore).not.toBe(originalInsertBefore);
    resetGuardState();
    expect(Element.prototype.appendChild).toBe(originalAppendChild);
    expect(Element.prototype.insertBefore).toBe(originalInsertBefore);
  });

  it('rewrites dynamically inserted Sourcepoint scripts', () => {
    installSourcepointGuard();

    const container = document.createElement('div');
    const script = document.createElement('script');
    script.src = 'https://cdn.privacy-mgmt.com/wrapperMessagingWithoutDetection.js';

    container.appendChild(script);

    expect(script.src).toContain(
      '/integrations/sourcepoint/cdn/wrapperMessagingWithoutDetection.js'
    );
    expect(script.src).not.toContain('cdn.privacy-mgmt.com');
  });

  it('does not rewrite unrelated scripts', () => {
    installSourcepointGuard();

    const container = document.createElement('div');
    const script = document.createElement('script');
    script.src = 'https://example.com/app.js';

    container.appendChild(script);

    expect(script.src).toBe('https://example.com/app.js');
  });
});
