import type { FirstDisplaySliceActivationContext } from '../../shared/first_display_transaction';
import type { FirstDisplaySliceId } from '../../kernel/release_catalog';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

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
  own: FirstDisplaySliceActivationContext['own'],
  config: unknown
) => unknown;

export interface PreparedInitialSlice {
  readonly activate: (context: FirstDisplaySliceActivationContext) => void;
}

export interface InitialSliceDefinition {
  readonly id: OptionalFirstDisplaySliceId;
  readonly prepare: (host: FirstDisplaySliceHost) => PreparedInitialSlice;
}

/** Define one release-owned initial slice without importing a persistent product owner. */
export function defineInitialSlice(
  id: OptionalFirstDisplaySliceId,
  install?: InitialSliceInstaller
): InitialSliceDefinition {
  return Object.freeze({
    id,
    prepare: (host: FirstDisplaySliceHost): PreparedInitialSlice => {
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
  return registerCurrentFirstDisplayComponent(definition.id, absoluteCatalogOrder, (host) =>
    definition.prepare(host as FirstDisplaySliceHost)
  );
}
