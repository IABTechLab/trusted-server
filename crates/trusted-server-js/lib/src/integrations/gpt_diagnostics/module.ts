import type { GoogletagDiagnosticsFact } from '../../adapters/googletag';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';
import { realmOwnedDocument } from '../../shared/realm';

import {
  GptDiagnosticsDataApiController,
  type GptDiagnosticsPresentationFactory,
} from './data_api';
import { GptDiagnosticsObserver } from './observer';
import { GptDiagnosticsStore } from './store';

export const GPT_DIAGNOSTICS_INTEGRATION_ID = 'gpt_diagnostics' as const;

interface GptEventsCapability {
  readonly subscribe: (listener: (fact: Readonly<Record<string, unknown>>) => void) => () => void;
}

function activeConfiguration(candidate: unknown): boolean {
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
    const active = Object.getOwnPropertyDescriptor(candidate, 'active');
    return Boolean(active?.enumerable && 'value' in active && active.value === true);
  } catch {
    return false;
  }
}

function frozenCapability(
  interfaces: Readonly<Record<string, unknown>>,
  key: string
): Readonly<Record<string, unknown>> | undefined {
  try {
    const descriptor = Object.getOwnPropertyDescriptor(interfaces, key);
    if (!descriptor || !('value' in descriptor)) return undefined;
    const candidate = descriptor.value;
    return typeof candidate === 'object' &&
      candidate !== null &&
      !Array.isArray(candidate) &&
      Object.isFrozen(candidate)
      ? (candidate as Readonly<Record<string, unknown>>)
      : undefined;
  } catch {
    return undefined;
  }
}

function gptEventsCapability(
  interfaces: Readonly<Record<string, unknown>>
): GptEventsCapability | undefined {
  const candidate = frozenCapability(interfaces, 'gpt.events.v1');
  if (!candidate || Reflect.ownKeys(candidate).length !== 1) return undefined;
  return typeof candidate['subscribe'] === 'function'
    ? (candidate as unknown as GptEventsCapability)
    : undefined;
}

/** Build the critical, data-only GPT diagnostics fact provider. */
export function createGptDiagnosticsIntegrationRegistration(
  releaseId: string
): IntegrationRegistration {
  return Object.freeze({
    abi: 1,
    id: GPT_DIAGNOSTICS_INTEGRATION_ID,
    phase: 'critical',
    releaseId,
    prepare: (context: IntegrationPrepareContext) => {
      if (!activeConfiguration(context.config)) {
        throw new TypeError('GPT diagnostics integration config is invalid');
      }
      const runtime = frozenCapability(context.interfaces, 'runtime.v1');
      const runtimeDocument = realmOwnedDocument(runtime?.['document']);
      const runtimeWindow = runtimeDocument?.defaultView;
      if (!runtime || !runtimeWindow) {
        throw new TypeError('GPT diagnostics requires runtime.v1');
      }
      const events = gptEventsCapability(context.interfaces);
      if (!events) throw new TypeError('GPT diagnostics requires gpt.events.v1');

      const store = new GptDiagnosticsStore();
      const observer = new GptDiagnosticsObserver(store);
      const controller = new GptDiagnosticsDataApiController(store, {
        location: runtimeWindow.location,
      });
      context.onDispose(() => controller.destroy());
      let active = false;
      const dataCapability = Object.freeze({
        api: controller.api,
        attachPresentation: (factory: GptDiagnosticsPresentationFactory): (() => void) =>
          controller.attachPresentation(factory),
      });

      return Object.freeze({
        activate: ({ onDispose }: IntegrationActivationContext) => {
          if (active) throw new Error('GPT diagnostics already activated');
          const ownership: { release?: () => void } = {};
          onDispose(() => {
            active = false;
            ownership.release?.();
          });
          active = true;
          const release = events.subscribe((fact) => {
            if (active) observer.consume(fact as unknown as Readonly<GoogletagDiagnosticsFact>);
          });
          if (typeof release !== 'function') {
            throw new TypeError('GPT diagnostics event disposer is unavailable');
          }
          ownership.release = release;
        },
        interfaces: Object.freeze({ 'gpt_diag.v1': dataCapability }),
      });
    },
  });
}
