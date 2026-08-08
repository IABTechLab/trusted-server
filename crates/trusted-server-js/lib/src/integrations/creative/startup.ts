import type { CreativeBootV1 } from '../../core/types';

export interface CreativeGuardHandle {
  readonly dispose: () => void;
  readonly scan: () => void;
}

export interface CreativeStartup {
  readonly activate: (config: Readonly<CreativeBootV1>) => () => void;
  readonly start: (config: Readonly<CreativeBootV1>) => void;
}

export interface CreativeStartupOptions {
  readonly document: {
    readonly readyState: DocumentReadyState;
    addEventListener(type: 'DOMContentLoaded', listener: () => void, options: { once: true }): void;
    removeEventListener(type: 'DOMContentLoaded', listener: () => void): void;
  };
  readonly installClickGuard: () => CreativeGuardHandle;
  readonly installDynamicIframeProxy: () => CreativeGuardHandle;
  readonly installDynamicImageProxy: () => CreativeGuardHandle;
}

function sameBoot(left: Readonly<CreativeBootV1>, right: Readonly<CreativeBootV1>): boolean {
  return (
    left.version === right.version &&
    left.enabled === right.enabled &&
    left.clickGuard === right.clickGuard &&
    left.renderGuard === right.renderGuard
  );
}

function validHandle(candidate: unknown): candidate is CreativeGuardHandle {
  return (
    typeof candidate === 'object' &&
    candidate !== null &&
    typeof Reflect.get(candidate, 'dispose') === 'function' &&
    typeof Reflect.get(candidate, 'scan') === 'function'
  );
}

/** Own creative guard installation separately from the post-commit initial scan. */
export function createCreativeStartup(options: CreativeStartupOptions): CreativeStartup {
  const handles: CreativeGuardHandle[] = [];
  let activated = false;
  let activatedBoot: Readonly<CreativeBootV1> | undefined;
  let readyListener: (() => void) | undefined;
  let released = false;
  let started = false;

  const scan = (): void => {
    if (released) return;
    for (let index = 0; index < handles.length; index += 1) {
      try {
        handles[index]?.scan();
      } catch {
        // One hostile guard scan cannot suppress the remaining active guards.
      }
    }
  };

  const disposeHandles = (): void => {
    for (let index = handles.length - 1; index >= 0; index -= 1) {
      try {
        handles[index]?.dispose();
      } catch {
        // Continue releasing every previously installed guard.
      }
    }
    handles.length = 0;
  };

  const install = (installer: () => CreativeGuardHandle): void => {
    const handle = installer();
    if (!validHandle(handle)) throw new TypeError('Creative guard handle is invalid');
    handles.push(handle);
  };

  return Object.freeze({
    activate: (config: Readonly<CreativeBootV1>): (() => void) => {
      if (activated || released) throw new Error('Creative startup is already activated');
      activated = true;
      activatedBoot = config;
      try {
        if (config.enabled && config.clickGuard) install(options.installClickGuard);
        if (config.enabled && config.renderGuard) {
          install(options.installDynamicImageProxy);
          install(options.installDynamicIframeProxy);
        }
        if (handles.length > 0 && options.document.readyState === 'loading') {
          readyListener = () => scan();
          options.document.addEventListener('DOMContentLoaded', readyListener, { once: true });
        }
      } catch (error) {
        const listener = readyListener;
        readyListener = undefined;
        try {
          if (listener) options.document.removeEventListener('DOMContentLoaded', listener);
        } catch {
          // Preserve the activation failure while completing owned guard rollback.
        } finally {
          disposeHandles();
        }
        throw error;
      }
      return (): void => {
        if (released) return;
        released = true;
        const listener = readyListener;
        readyListener = undefined;
        try {
          if (listener) options.document.removeEventListener('DOMContentLoaded', listener);
        } finally {
          disposeHandles();
        }
      };
    },
    start: (config: Readonly<CreativeBootV1>): void => {
      if (started) throw new Error('Creative startup is already started');
      started = true;
      if (released) return;
      if (!activated || !activatedBoot || !sameBoot(activatedBoot, config)) {
        throw new Error('Creative startup is unavailable');
      }
      if (handles.length > 0 && options.document.readyState !== 'loading') scan();
    },
  });
}
