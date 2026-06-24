import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { installDidomiSdkProxy } from '../../../src/integrations/didomi';

const ORIGINAL_WINDOW = global.window;

type TestDidomiWindow = Window & {
  didomiConfig?: any;
  __tsjs_didomi?: { proxyPath?: string };
};

function createWindow(url: string) {
  return {
    location: new URL(url) as unknown as Location,
  } as TestDidomiWindow;
}

describe('integrations/didomi', () => {
  let testWindow: ReturnType<typeof createWindow>;

  beforeEach(() => {
    testWindow = createWindow('https://example.com/page');
    Object.assign(globalThis as any, { window: testWindow });
  });

  afterEach(() => {
    Object.assign(globalThis as any, { window: ORIGINAL_WINDOW });
  });

  it('initializes didomiConfig and forces sdkPath through trusted server proxy', () => {
    installDidomiSdkProxy();

    expect(testWindow.didomiConfig).toBeDefined();
    expect(testWindow.didomiConfig.sdkPath).toBe(
      'https://example.com/integrations/didomi/consent/'
    );
  });

  it('preserves existing config fields while overriding sdkPath', () => {
    testWindow.didomiConfig = { apiKey: 'abc', sdkPath: 'https://sdk.privacy-center.org/' };

    installDidomiSdkProxy();

    expect(testWindow.didomiConfig.apiKey).toBe('abc');
    expect(testWindow.didomiConfig.sdkPath).toBe(
      'https://example.com/integrations/didomi/consent/'
    );
  });

  it('uses the server-injected custom proxy path', () => {
    testWindow.__tsjs_didomi = { proxyPath: '/my-custom-consent/' };

    installDidomiSdkProxy();

    expect(testWindow.didomiConfig.sdkPath).toBe('https://example.com/my-custom-consent/');
  });
});
