import type { FirstDisplaySliceActivationContext } from '../transaction';

interface DidomiConfig {
  sdkPath?: string;
  [key: string]: unknown;
}

interface DidomiTarget {
  didomiConfig?: DidomiConfig;
  readonly location: { readonly origin: string };
}

interface DidomiInitialBindings {
  readonly config: Readonly<{ proxyPath: string }>;
  readonly observe: (name: 'sdk_path', value: string) => void;
  readonly target: DidomiTarget;
}

function exactDataRecord(value: unknown, keys: readonly string[]): Record<string, unknown> | undefined {
  try {
    if (
      typeof value !== 'object' ||
      value === null ||
      Array.isArray(value) ||
      Object.getPrototypeOf(value) !== Object.prototype ||
      !Object.isFrozen(value)
    ) {
      return undefined;
    }
    const ownKeys = Reflect.ownKeys(value);
    if (
      ownKeys.length !== keys.length ||
      !ownKeys.every((key) => typeof key === 'string' && keys.includes(key))
    ) {
      return undefined;
    }
    const result: Record<string, unknown> = {};
    for (const key of keys) {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
      result[key] = descriptor.value;
    }
    return result;
  } catch {
    return undefined;
  }
}

function snapshotBindings(candidate: unknown): DidomiInitialBindings | undefined {
  const fields = exactDataRecord(candidate, ['config', 'observe', 'target']);
  const config = exactDataRecord(fields?.config, ['proxyPath']);
  if (
    !fields ||
    !config ||
    typeof config.proxyPath !== 'string' ||
    !config.proxyPath.startsWith('/') ||
    config.proxyPath.startsWith('//') ||
    config.proxyPath.startsWith('/\\') ||
    config.proxyPath.length > 2_048 ||
    config.proxyPath.includes('?') ||
    config.proxyPath.includes('#') ||
    typeof fields.observe !== 'function' ||
    typeof fields.target !== 'object' ||
    fields.target === null
  ) {
    return undefined;
  }
  const target = fields.target as DidomiTarget;
  if (typeof target.location?.origin !== 'string') return undefined;
  return {
    config: Object.freeze({ proxyPath: config.proxyPath }),
    observe: fields.observe as DidomiInitialBindings['observe'],
    target,
  };
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

/** Install only Didomi's same-origin parser-time SDK path and own its rollback. */
export function installDidomiInitial(
  candidate: unknown,
  own: FirstDisplaySliceActivationContext['own']
): void {
  const bindings = snapshotBindings(candidate);
  if (!bindings || typeof own !== 'function') throw new TypeError('invalid Didomi initial bindings');
  const { config, observe, target } = bindings;
  const origin = new URL(target.location.origin).origin;
  const parsed = new URL(config.proxyPath, origin);
  if (parsed.origin !== origin || parsed.username !== '' || parsed.password !== '') {
    throw new TypeError('Didomi proxy path must remain on the publisher origin');
  }
  const installedPath = `${parsed.origin}${parsed.pathname}`;
  const previousTargetDescriptor = Object.getOwnPropertyDescriptor(target, 'didomiConfig');
  if (previousTargetDescriptor && !('value' in previousTargetDescriptor)) {
    throw new TypeError('Didomi publisher config accessor is unsupported');
  }
  let publisherConfig = previousTargetDescriptor?.value as DidomiConfig | undefined;
  const created = publisherConfig === undefined;
  if (created) {
    publisherConfig = {};
    if (!Reflect.set(target, 'didomiConfig', publisherConfig)) {
      throw new TypeError('Didomi publisher config is not writable');
    }
  }
  if (typeof publisherConfig !== 'object' || publisherConfig === null) {
    throw new TypeError('Didomi publisher config is invalid');
  }
  const previousSdkDescriptor = Object.getOwnPropertyDescriptor(publisherConfig, 'sdkPath');
  if (previousSdkDescriptor && !('value' in previousSdkDescriptor)) {
    throw new TypeError('Didomi sdkPath accessor is unsupported');
  }
  if (!Reflect.set(publisherConfig, 'sdkPath', installedPath)) {
    throw new TypeError('Didomi sdkPath is not writable');
  }
  const installedSdkDescriptor = Object.getOwnPropertyDescriptor(publisherConfig, 'sdkPath');
  let active = true;
  own(() => {
    if (!active) return;
    active = false;
    try {
      if (target.didomiConfig !== publisherConfig) return;
      if (!sameDescriptor(Object.getOwnPropertyDescriptor(publisherConfig, 'sdkPath'), installedSdkDescriptor)) {
        return;
      }
      if (previousSdkDescriptor) {
        Object.defineProperty(publisherConfig, 'sdkPath', previousSdkDescriptor);
      } else {
        Reflect.deleteProperty(publisherConfig, 'sdkPath');
      }
      if (created && Reflect.ownKeys(publisherConfig).length === 0) {
        if (previousTargetDescriptor) {
          Object.defineProperty(target, 'didomiConfig', previousTargetDescriptor);
        } else {
          Reflect.deleteProperty(target, 'didomiConfig');
        }
      }
    } catch {
      // Publisher replacement wins over provisional rollback.
    }
  });
  observe('sdk_path', installedPath);
}
