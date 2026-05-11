import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

type SourcepointWindow = Window & {
  __tsjs_sourcepoint?: {
    rewriteSdk?: boolean;
  };
  __tsjs_installSourcepointGuard?: unknown;
};

describe('Sourcepoint integration initialization', () => {
  let win: SourcepointWindow;

  beforeEach(async () => {
    win = window as SourcepointWindow;
    delete win.__tsjs_sourcepoint;

    const guard = await import('../../../src/integrations/sourcepoint/script_guard');
    guard.resetGuardState();
  });

  afterEach(async () => {
    const guard = await import('../../../src/integrations/sourcepoint/script_guard');
    guard.resetGuardState();
    delete win.__tsjs_sourcepoint;
    delete win.__tsjs_installSourcepointGuard;
  });

  it('installs the guard when rewriteSdk is enabled', async () => {
    vi.resetModules();
    win.__tsjs_sourcepoint = { rewriteSdk: true };

    const guard = await import('../../../src/integrations/sourcepoint/script_guard');
    await import('../../../src/integrations/sourcepoint/index');

    expect(guard.isGuardInstalled()).toBe(true);
  });

  it('skips the guard when rewriteSdk is disabled', async () => {
    vi.resetModules();
    win.__tsjs_sourcepoint = { rewriteSdk: false };

    const guard = await import('../../../src/integrations/sourcepoint/script_guard');
    await import('../../../src/integrations/sourcepoint/index');

    expect(guard.isGuardInstalled()).toBe(false);
  });

  it('defaults to installing the guard when rewriteSdk is missing for backward compatibility', async () => {
    vi.resetModules();

    const guard = await import('../../../src/integrations/sourcepoint/script_guard');
    await import('../../../src/integrations/sourcepoint/index');

    expect(guard.isGuardInstalled()).toBe(true);
  });
});
