import type { FirstDisplaySliceActivationContext } from '../transaction';
import type { FirstDisplaySliceId } from '../../kernel/release_catalog';

export type OptionalFirstDisplaySliceId = Exclude<FirstDisplaySliceId, 'first_display'>;

export interface FirstDisplaySliceHost {
  readonly activate: (
    id: OptionalFirstDisplaySliceId,
    own: FirstDisplaySliceActivationContext['own']
  ) => void;
}

export interface PreparedInitialSlice {
  readonly activate: (context: FirstDisplaySliceActivationContext) => void;
}

export interface InitialSliceDefinition {
  readonly id: OptionalFirstDisplaySliceId;
  readonly prepare: (host: FirstDisplaySliceHost) => PreparedInitialSlice;
}

function validHost(host: FirstDisplaySliceHost): boolean {
  try {
    if (
      typeof host !== 'object' ||
      host === null ||
      Array.isArray(host) ||
      Object.getPrototypeOf(host) !== Object.prototype ||
      !Object.isFrozen(host)
    ) {
      return false;
    }
    const keys = Reflect.ownKeys(host);
    if (keys.length !== 1 || keys[0] !== 'activate') return false;
    const descriptor = Object.getOwnPropertyDescriptor(host, 'activate');
    return Boolean(descriptor?.enumerable && 'value' in descriptor && typeof descriptor.value === 'function');
  } catch {
    return false;
  }
}

/** Define one release-owned initial slice without importing a persistent product owner. */
export function defineInitialSlice(id: OptionalFirstDisplaySliceId): InitialSliceDefinition {
  return Object.freeze({
    id,
    prepare: (host: FirstDisplaySliceHost): PreparedInitialSlice => {
      if (!validHost(host)) throw new TypeError(`Invalid ${id} host`);
      return Object.freeze({
        activate: (context: FirstDisplaySliceActivationContext): void => {
          host.activate(id, context.own);
        },
      });
    },
  });
}
