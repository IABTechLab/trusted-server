import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';

export const PREBID_INTEGRATION_ID = 'prebid' as const;

const MAX_CONFIG_DEPTH = 16;
const MAX_CONFIG_NODES = 512;
const MAX_CONFIG_MEMBERS = 256;
const arrayIsArrayIntrinsic = Array.isArray;
const numberIsFiniteIntrinsic = Number.isFinite;
const objectGetOwnPropertyDescriptorIntrinsic = Object.getOwnPropertyDescriptor;
const objectGetOwnPropertyNamesIntrinsic = Object.getOwnPropertyNames;
const objectGetOwnPropertySymbolsIntrinsic = Object.getOwnPropertySymbols;
const objectGetPrototypeOfIntrinsic = Object.getPrototypeOf;
const objectIsFrozenIntrinsic = Object.isFrozen;

interface PrebidIntegrationRuntime {
  readonly start: (config: unknown) => void;
}

function validFrozenConfig(candidate: unknown): boolean {
  const seen = new Set<object>();
  let nodes = 0;
  const visit = (value: unknown, depth: number, topLevel: boolean): boolean => {
    if (value === undefined) return topLevel;
    if (value === null || typeof value === 'string' || typeof value === 'boolean') return true;
    if (typeof value === 'number') return numberIsFiniteIntrinsic(value);
    if (typeof value !== 'object' || depth > MAX_CONFIG_DEPTH || nodes >= MAX_CONFIG_NODES) {
      return false;
    }
    if (seen.has(value) || !objectIsFrozenIntrinsic(value)) return false;
    seen.add(value);
    nodes += 1;

    const array = arrayIsArrayIntrinsic(value);
    const prototype = objectGetPrototypeOfIntrinsic(value);
    if (
      (!array && prototype !== Object.prototype && prototype !== null) ||
      (array && prototype !== Array.prototype)
    ) {
      return false;
    }
    if (objectGetOwnPropertySymbolsIntrinsic(value).length !== 0) return false;
    const names = objectGetOwnPropertyNamesIntrinsic(value);
    if (names.length > MAX_CONFIG_MEMBERS + (array ? 1 : 0)) return false;
    if (array) {
      const length = objectGetOwnPropertyDescriptorIntrinsic(value, 'length');
      if (!length || !('value' in length) || names.length !== length.value + 1) return false;
      for (let index = 0; index < length.value; index += 1) {
        const descriptor = objectGetOwnPropertyDescriptorIntrinsic(value, String(index));
        if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return false;
        if (!visit(descriptor.value, depth + 1, false)) return false;
      }
      return true;
    }

    for (let index = 0; index < names.length; index += 1) {
      const name = names[index];
      if (name === undefined) return false;
      const descriptor = objectGetOwnPropertyDescriptorIntrinsic(value, name);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return false;
      if (!visit(descriptor.value, depth + 1, false)) return false;
    }
    return true;
  };

  try {
    return visit(candidate, 0, true);
  } catch {
    return false;
  }
}

function readPrebidRuntime(
  interfaces: Readonly<Record<string, unknown>>
): PrebidIntegrationRuntime | undefined {
  try {
    const descriptor = objectGetOwnPropertyDescriptorIntrinsic(interfaces, PREBID_INTEGRATION_ID);
    if (!descriptor || !('value' in descriptor)) return undefined;
    const candidate = descriptor.value;
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      arrayIsArrayIntrinsic(candidate) ||
      !objectIsFrozenIntrinsic(candidate) ||
      Reflect.ownKeys(candidate).length !== 1
    ) {
      return undefined;
    }
    const start = objectGetOwnPropertyDescriptorIntrinsic(candidate, 'start');
    if (!start || !('value' in start) || typeof start.value !== 'function') return undefined;
    return candidate as PrebidIntegrationRuntime;
  } catch {
    return undefined;
  }
}

/** Build the release-bound Prebid module registered by the coordinated runtime. */
export function createPrebidIntegrationRegistration(release: string): IntegrationRegistration {
  return Object.freeze({
    id: PREBID_INTEGRATION_ID,
    release,
    prepare: ({ config, interfaces }: IntegrationPrepareContext) => {
      if (!validFrozenConfig(config)) throw new TypeError('Prebid integration config is invalid');
      const runtime = readPrebidRuntime(interfaces);
      if (!runtime) throw new TypeError('Prebid integration runtime is unavailable');

      return Object.freeze({
        activate: ({ afterCommit }: IntegrationActivationContext) => {
          afterCommit(() => runtime.start(config));
        },
      });
    },
  });
}
