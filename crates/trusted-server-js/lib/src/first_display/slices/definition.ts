import type { FirstDisplaySliceActivationContext } from '../../shared/first_display_transaction';
import type { FirstDisplaySliceId } from '../../kernel/release_catalog';

export type OptionalFirstDisplaySliceId = Exclude<FirstDisplaySliceId, 'first_display'>;

export interface FirstDisplaySliceHost {
  readonly activate: (
    id: OptionalFirstDisplaySliceId,
    own: FirstDisplaySliceActivationContext['own'],
    install: InitialSliceInstaller
  ) => void;
}

export type InitialSliceInstaller = (
  bindings: unknown,
  own: FirstDisplaySliceActivationContext['own'],
  config: unknown
) => unknown;

export interface InitialSliceDefinition {
  readonly id: OptionalFirstDisplaySliceId;
  readonly install: InitialSliceInstaller;
}
