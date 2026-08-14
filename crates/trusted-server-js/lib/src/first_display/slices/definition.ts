import type { FirstDisplaySliceActivationContext } from '../transaction';
import type { FirstDisplaySliceId } from '../../kernel/release_catalog';
import {
  firstDisplayComponentRegistration,
  registerCurrentFirstDisplayComponent,
} from '../registration';

export type OptionalFirstDisplaySliceId = Exclude<FirstDisplaySliceId, 'first_display'>;

export interface FirstDisplaySliceHost {
  readonly activate: (
    id: OptionalFirstDisplaySliceId,
    own: FirstDisplaySliceActivationContext['own'],
    install?: InitialSliceInstaller
  ) => void;
}

export type InitialSliceInstaller = (
  bindings: unknown,
  own: FirstDisplaySliceActivationContext['own']
) => void;

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
    return Boolean(
      descriptor?.enumerable && 'value' in descriptor && typeof descriptor.value === 'function'
    );
  } catch {
    return false;
  }
}

/** Define one release-owned initial slice without importing a persistent product owner. */
export function defineInitialSlice(
  id: OptionalFirstDisplaySliceId,
  install?: InitialSliceInstaller
): InitialSliceDefinition {
  return Object.freeze({
    id,
    prepare: (host: FirstDisplaySliceHost): PreparedInitialSlice => {
      if (!validHost(host)) throw new TypeError(`Invalid ${id} host`);
      return Object.freeze({
        activate: (context: FirstDisplaySliceActivationContext): void => {
          host.activate(id, context.own, install);
        },
      });
    },
  });
}

/** Register one selected optional slice into the bootstrap-owned artifact transaction. */
export function registerInitialSlice(
  definition: InitialSliceDefinition,
  absoluteCatalogOrder: number
): boolean {
  return registerCurrentFirstDisplayComponent(
    firstDisplayComponentRegistration(definition.id, absoluteCatalogOrder, (host) =>
      definition.prepare(host as FirstDisplaySliceHost)
    )
  );
}
