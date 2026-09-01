import { describe, expect, it, vi } from 'vitest';

import type { CreativeBootV1 } from '../../../src/core/types';
import {
  createCreativeStartup,
  type CreativeGuardHandle,
} from '../../../src/integrations/creative/startup';

function config(overrides: Partial<CreativeBootV1> = {}): Readonly<CreativeBootV1> {
  return Object.freeze({
    version: 1 as const,
    enabled: true,
    clickGuard: true,
    renderGuard: true,
    ...overrides,
  });
}

function guard(name: string, order: string[]): CreativeGuardHandle {
  return Object.freeze({
    dispose: vi.fn(() => order.push(`dispose:${name}`)),
    scan: vi.fn(() => order.push(`scan:${name}`)),
  });
}

function readyDocument(readyState: DocumentReadyState = 'complete') {
  let listener: (() => void) | undefined;
  return {
    document: {
      readyState,
      addEventListener: vi.fn(
        (_type: 'DOMContentLoaded', next: () => void, _options: { once: true }) => {
          listener = next;
        }
      ),
      removeEventListener: vi.fn((_type: 'DOMContentLoaded', candidate: () => void) => {
        if (listener === candidate) listener = undefined;
      }),
    },
    dispatchReady: (): void => {
      const current = listener;
      listener = undefined;
      current?.();
    },
  };
}

describe('creative startup ownership', () => {
  it('installs selected guards synchronously, scans after commit, and disposes in reverse', async () => {
    const order: string[] = [];
    const click = guard('click', order);
    const image = guard('image', order);
    const iframe = guard('iframe', order);
    const target = readyDocument();
    const startup = createCreativeStartup({
      document: target.document,
      installClickGuard: vi.fn(() => (order.push('install:click'), click)),
      installDynamicImageProxy: vi.fn(() => (order.push('install:image'), image)),
      installDynamicIframeProxy: vi.fn(() => (order.push('install:iframe'), iframe)),
    });
    const boot = config();

    const release = startup.activate(boot);
    expect(order).toEqual(['install:click', 'install:image', 'install:iframe']);

    startup.start(boot);
    expect(order).toEqual([
      'install:click',
      'install:image',
      'install:iframe',
      'scan:click',
      'scan:image',
      'scan:iframe',
    ]);

    release();
    release();
    expect(order.slice(-3)).toEqual(['dispose:iframe', 'dispose:image', 'dispose:click']);
  });

  it('owns one loading-document rescan and removes it on disposal', async () => {
    const order: string[] = [];
    const click = guard('click', order);
    const target = readyDocument('loading');
    const startup = createCreativeStartup({
      document: target.document,
      installClickGuard: () => click,
      installDynamicImageProxy: () => guard('image', order),
      installDynamicIframeProxy: () => guard('iframe', order),
    });
    const boot = config({ renderGuard: false });

    const release = startup.activate(boot);
    expect(target.document.addEventListener).toHaveBeenCalledExactlyOnceWith(
      'DOMContentLoaded',
      expect.any(Function),
      { once: true }
    );
    startup.start(boot);
    expect(click.scan).not.toHaveBeenCalled();

    target.dispatchReady();
    target.dispatchReady();
    expect(click.scan).toHaveBeenCalledTimes(1);

    release();
    expect(target.document.removeEventListener).toHaveBeenCalledTimes(1);
    expect(click.dispose).toHaveBeenCalledTimes(1);
  });

  it('rolls back earlier guards when a later installer throws', async () => {
    const order: string[] = [];
    const click = guard('click', order);
    const image = guard('image', order);
    const target = readyDocument();
    const startup = createCreativeStartup({
      document: target.document,
      installClickGuard: () => click,
      installDynamicImageProxy: () => image,
      installDynamicIframeProxy: () => {
        throw new Error('fictional iframe installation failure');
      },
    });

    expect(() => startup.activate(config())).toThrow('fictional iframe installation failure');
    expect(order).toEqual(['dispose:image', 'dispose:click']);
  });

  it('removes an exact ready listener when hostile registration throws after installing it', () => {
    const order: string[] = [];
    const click = guard('click', order);
    let listener: (() => void) | undefined;
    const document = {
      readyState: 'loading' as const,
      addEventListener: vi.fn(
        (_type: 'DOMContentLoaded', candidate: () => void, _options: { once: true }) => {
          listener = candidate;
          throw new Error('fictional ready listener registration failure');
        }
      ),
      removeEventListener: vi.fn((_type: 'DOMContentLoaded', candidate: () => void) => {
        if (listener === candidate) listener = undefined;
      }),
    };
    const startup = createCreativeStartup({
      document,
      installClickGuard: () => click,
      installDynamicImageProxy: () => guard('image', order),
      installDynamicIframeProxy: () => guard('iframe', order),
    });

    expect(() => startup.activate(config({ renderGuard: false }))).toThrow(
      'fictional ready listener registration failure'
    );
    expect(document.removeEventListener).toHaveBeenCalledExactlyOnceWith(
      'DOMContentLoaded',
      expect.any(Function)
    );
    expect(click.dispose).toHaveBeenCalledTimes(1);

    listener?.();
    expect(click.scan).not.toHaveBeenCalled();
  });

  it('contains hostile scans and still visits every active guard', async () => {
    const order: string[] = [];
    const click = guard('click', order);
    const image = guard('image', order);
    const iframe = guard('iframe', order);
    vi.mocked(click.scan).mockImplementation(() => {
      order.push('scan:click');
      throw new Error('fictional click scan failure');
    });
    const target = readyDocument();
    const startup = createCreativeStartup({
      document: target.document,
      installClickGuard: () => click,
      installDynamicImageProxy: () => image,
      installDynamicIframeProxy: () => iframe,
    });
    const boot = config();
    startup.activate(boot);

    expect(() => startup.start(boot)).not.toThrow();
    expect(image.scan).toHaveBeenCalledTimes(1);
    expect(iframe.scan).toHaveBeenCalledTimes(1);
  });

  it('prevents a late start after release and rejects duplicate lifecycle calls', async () => {
    const order: string[] = [];
    const click = guard('click', order);
    const target = readyDocument();
    const startup = createCreativeStartup({
      document: target.document,
      installClickGuard: () => click,
      installDynamicImageProxy: () => guard('image', order),
      installDynamicIframeProxy: () => guard('iframe', order),
    });
    const boot = config({ renderGuard: false });
    const release = startup.activate(boot);
    expect(() => startup.activate(boot)).toThrow('already activated');
    release();

    startup.start(boot);
    expect(click.scan).not.toHaveBeenCalled();
    expect(() => startup.start(boot)).toThrow('already started');
  });
});
