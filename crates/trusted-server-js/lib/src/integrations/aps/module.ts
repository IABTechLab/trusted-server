import type { MessagingAdapter } from '../../adapters/messaging';
import { isEmptyIntegrationConfigV1 } from '../../shared/integration_config_validators';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';
import type {
  BootstrapNonceRegistry,
  RendererNonceRegistry,
  RenderAttempt,
} from '../../services/render';
import { validatePersistentFirstDisplaySliceAdoptionV1 } from '../../shared/takeover';
import type { PucApsMountInput } from '../../services/puc_bridge';

import {
  renderDirectApsAttempt,
  renderPucApsAttempt,
  resolveApsRendererV1Url,
  validateApsRenderer,
} from './render';

interface RenderCapability {
  readonly bootstrapNonces: BootstrapNonceRegistry;
  readonly publisherOrigin: string;
  readonly rendererNonces: RendererNonceRegistry;
  readonly registerRenderer: (
    type: 'aps',
    renderer: (attempt: RenderAttempt, container: HTMLElement) => boolean
  ) => () => void;
}

interface MessagesCapability {
  readonly messaging: MessagingAdapter;
  readonly registerApsValidation: (
    validation: Readonly<{
      readonly expectedPublisherOrigin: string;
      readonly expectedRendererUrl: string;
      readonly validateApsRenderer: (candidate: unknown) => boolean;
    }>
  ) => () => void;
}

function capability<Value extends object>(
  interfaces: Readonly<Record<string, unknown>>,
  key: string
): Value {
  const value = interfaces[key];
  if (typeof value !== 'object' || value === null || !Object.isFrozen(value)) {
    throw new TypeError(`APS requires ${key}`);
  }
  return value as Value;
}

/** APS owns only its renderer implementation; shared services remain provider capabilities. */
export function createApsIntegrationRegistration(releaseId: string): IntegrationRegistration {
  const prepare = (context: IntegrationPrepareContext) => {
    if (!isEmptyIntegrationConfigV1(context.config)) {
      throw new TypeError('APS integration config is invalid');
    }
    const render = capability<RenderCapability>(context.interfaces, 'render.v1');
    const messages = capability<MessagesCapability>(context.interfaces, 'messages.v1');
    if (
      typeof render.registerRenderer !== 'function' ||
      typeof render.publisherOrigin !== 'string' ||
      typeof messages.messaging !== 'object' ||
      messages.messaging === null ||
      typeof messages.registerApsValidation !== 'function'
    ) {
      throw new TypeError('APS capability graph is malformed');
    }
    const rendererUrl = resolveApsRendererV1Url(render.publisherOrigin);
    if (!rendererUrl) throw new TypeError('APS publisher origin is invalid');
    let active = false;
    const renderer = (attempt: RenderAttempt, container: HTMLElement): boolean =>
      active &&
      renderDirectApsAttempt({
        attempt,
        bootstrapNonces: render.bootstrapNonces,
        container,
        messaging: messages.messaging,
        nonces: render.rendererNonces,
        publisherOrigin: render.publisherOrigin,
      });
    const renderPuc = (input: PucApsMountInput): boolean =>
      active &&
      renderPucApsAttempt({
        ...input,
        bootstrapNonces: render.bootstrapNonces,
        messaging: messages.messaging,
        nonces: render.rendererNonces,
        publisherOrigin: render.publisherOrigin,
      });
    const apsCapability = Object.freeze({ render: renderer, renderPuc });
    const validation = Object.freeze({
      expectedPublisherOrigin: render.publisherOrigin,
      expectedRendererUrl: rendererUrl,
      validateApsRenderer: (candidate: unknown): boolean =>
        validateApsRenderer(candidate, render.publisherOrigin) !== undefined,
    });
    context.onDispose(() => {
      active = false;
    });
    return Object.freeze({
      activate: (activation: IntegrationActivationContext) => {
        if (active) throw new Error('APS already activated');
        if (
          activation.adoption !== undefined &&
          !validatePersistentFirstDisplaySliceAdoptionV1(
            activation.adoption,
            'aps_initial',
            (state) =>
              state.values.length === 1 &&
              state.values[0]?.[0] === 'protocol_version' &&
              state.values[0][1] === 1
          )
        ) {
          throw new TypeError('APS first-display parser state is invalid');
        }
        const validationRelease: { current?: () => void } = {};
        const rendererRelease: { current?: () => void } = {};
        activation.onDispose(() => validationRelease.current?.());
        activation.onDispose(() => rendererRelease.current?.());
        activation.onDispose(() => {
          active = false;
        });
        validationRelease.current = messages.registerApsValidation(validation);
        rendererRelease.current = render.registerRenderer('aps', renderer);
        active = true;
      },
      interfaces: Object.freeze({
        'aps.v1': apsCapability,
      }),
    });
  };
  return Object.freeze({
    abi: 1 as const,
    id: 'aps',
    phase: 'takeover' as const,
    releaseId,
    prepareSync: prepare,
    prepare,
  });
}
