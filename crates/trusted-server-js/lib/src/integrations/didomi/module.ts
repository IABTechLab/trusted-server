import type { IntegrationRegistration } from '../../kernel/integration_registry';
import {
  createLifecycleIntegrationRegistration,
  type IntegrationLifecycleRuntime,
} from '../../kernel/lifecycle_module';
import { log } from '../../core/log';

export const DIDOMI_INTEGRATION_ID = 'didomi' as const;

interface DidomiConfig {
  sdkPath?: string;
  [key: string]: unknown;
}

export interface DidomiRuntimeTarget {
  didomiConfig?: DidomiConfig;
  readonly location: { readonly href?: string; readonly origin?: string };
}

export interface DidomiRuntimeDependencies {
  readonly started: () => void;
  readonly target: DidomiRuntimeTarget;
}

function didomiBootConfig(candidate: unknown): candidate is Readonly<{ proxyPath: string }> {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      !Object.isFrozen(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      Reflect.ownKeys(candidate).length !== 1
    ) {
      return false;
    }
    const descriptor = Object.getOwnPropertyDescriptor(candidate, 'proxyPath');
    return Boolean(
      descriptor?.enumerable &&
        'value' in descriptor &&
        typeof descriptor.value === 'string' &&
        descriptor.value.startsWith('/') &&
        !descriptor.value.startsWith('//') &&
        !descriptor.value.startsWith('/\\') &&
        descriptor.value.length <= 2_048 &&
      !descriptor.value.includes('?') &&
      !descriptor.value.includes('#')
    );
  } catch {
    return false;
  }
}

function sameDescriptor(
  left: PropertyDescriptor | undefined,
  right: PropertyDescriptor | undefined
): boolean {
  return Boolean(
    left &&
    right &&
    'value' in left &&
    'value' in right &&
    left.value === right.value &&
    left.configurable === right.configurable &&
    left.enumerable === right.enumerable &&
    left.writable === right.writable
  );
}

/** Own only Didomi's proxied `sdkPath`, preserving all publisher configuration. */
export function createDidomiRuntime(
  dependencies: DidomiRuntimeDependencies = {
    started: () => log.info('Didomi integration initialized'),
    target: window as DidomiRuntimeTarget,
  }
): IntegrationLifecycleRuntime {
  return Object.freeze({
    activate: (candidate: unknown): (() => void) => {
      if (!didomiBootConfig(candidate)) throw new TypeError('Didomi config is invalid');
      const base = dependencies.target.location.origin ?? dependencies.target.location.href;
      if (!base) throw new TypeError('Didomi publisher origin is unavailable');
      const parsed = new URL(candidate.proxyPath, base);
      if (parsed.origin !== new URL(base).origin) {
        throw new TypeError('Didomi proxy path must remain on the publisher origin');
      }
      const installedPath = `${parsed.origin}${parsed.pathname}`;
      const previousTargetDescriptor = Object.getOwnPropertyDescriptor(
        dependencies.target,
        'didomiConfig'
      );
      let config = dependencies.target.didomiConfig;
      const created = config === undefined;
      if (created) {
        config = {};
        if (!Reflect.set(dependencies.target, 'didomiConfig', config)) {
          throw new TypeError('Didomi publisher config is not writable');
        }
      }
      if (typeof config !== 'object' || config === null) {
        throw new TypeError('Didomi publisher config is invalid');
      }
      const previousSdkDescriptor = Object.getOwnPropertyDescriptor(config, 'sdkPath');
      if (previousSdkDescriptor && !('value' in previousSdkDescriptor)) {
        throw new TypeError('Didomi sdkPath accessor is unsupported');
      }
      if (!Reflect.set(config, 'sdkPath', installedPath)) {
        throw new TypeError('Didomi sdkPath is not writable');
      }
      const installedSdkDescriptor = Object.getOwnPropertyDescriptor(config, 'sdkPath');
      let active = true;
      return (): void => {
        if (!active) return;
        active = false;
        try {
          if (dependencies.target.didomiConfig !== config) return;
          const current = Object.getOwnPropertyDescriptor(config, 'sdkPath');
          if (!sameDescriptor(current, installedSdkDescriptor)) return;
          if (previousSdkDescriptor)
            Object.defineProperty(config, 'sdkPath', previousSdkDescriptor);
          else Reflect.deleteProperty(config, 'sdkPath');
          if (
            created &&
            Reflect.ownKeys(config).length === 0 &&
            Object.getOwnPropertyDescriptor(dependencies.target, 'didomiConfig')?.value === config
          ) {
            if (previousTargetDescriptor) {
              Object.defineProperty(dependencies.target, 'didomiConfig', previousTargetDescriptor);
            } else {
              Reflect.deleteProperty(dependencies.target, 'didomiConfig');
            }
          }
        } catch {
          // Publisher replacement wins over cleanup.
        }
      };
    },
    start: (_config: unknown): void => dependencies.started(),
  });
}

export function createDidomiIntegrationRegistration(release: string): IntegrationRegistration {
  return createLifecycleIntegrationRegistration(DIDOMI_INTEGRATION_ID, release, {
    validateConfig: didomiBootConfig,
  });
}
