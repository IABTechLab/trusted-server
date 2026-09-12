import type { CreativeBootV1 } from '../../core/types';
import {
  createCreativeStartup,
  type CreativeStartupOptions,
} from '../../integrations/creative/startup';
import type { FirstDisplaySliceActivationContext } from '../../shared/first_display_transaction';

interface CreativeInitialBindings extends Pick<
  CreativeStartupOptions,
  'document' | 'installClickGuard' | 'installDynamicIframeProxy' | 'installDynamicImageProxy'
> {
  readonly observe: (name: 'guard_count', value: number) => void;
}

function exactRecord(value: unknown, keys: readonly string[]): Record<string, unknown> | undefined {
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

function snapshotConfig(candidate: unknown): Readonly<CreativeBootV1> | undefined {
  const config = exactRecord(candidate, ['version', 'enabled', 'clickGuard', 'renderGuard']);
  if (
    !config ||
    config.version !== 1 ||
    config.enabled !== true ||
    typeof config.clickGuard !== 'boolean' ||
    typeof config.renderGuard !== 'boolean' ||
    (!config.clickGuard && !config.renderGuard)
  ) {
    return undefined;
  }
  return Object.freeze({
    version: 1,
    enabled: true,
    clickGuard: config.clickGuard,
    renderGuard: config.renderGuard,
  });
}

/** Install the selected parser-time creative guards under the agent rollback owner. */
export function installCreativeInitial(
  candidate: unknown,
  own: FirstDisplaySliceActivationContext['own'],
  configCandidate: unknown
): void {
  const bindings = candidate as CreativeInitialBindings;
  const config = snapshotConfig(configCandidate);
  if (!config || typeof own !== 'function') throw new TypeError('tsjs');
  const startup = createCreativeStartup({
    document: bindings.document,
    installClickGuard: bindings.installClickGuard,
    installDynamicIframeProxy: bindings.installDynamicIframeProxy,
    installDynamicImageProxy: bindings.installDynamicImageProxy,
  });
  const release = startup.activate(config);
  own(release);
  bindings.observe('guard_count', (config.clickGuard ? 1 : 0) + (config.renderGuard ? 2 : 0));
}
