import type { CreativeBootV1 } from '../../core/types';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';
import type { RuntimeCapabilityV1 } from '../../kernel/runtime';

import { installClickGuard } from './click';
import { installDynamicIframeProxy } from './iframe';
import { installDynamicImageProxy } from './image';
import { createCreativeStartup } from './startup';

export const CREATIVE_INTEGRATION_ID = 'creative' as const;

function readCreativeBoot(candidate: unknown): Readonly<CreativeBootV1> | undefined {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      !Object.isFrozen(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      Object.getOwnPropertySymbols(candidate).length !== 0
    ) {
      return undefined;
    }
    const keys = Object.getOwnPropertyNames(candidate).sort();
    const expected = ['clickGuard', 'enabled', 'renderGuard', 'version'];
    if (keys.length !== expected.length || keys.some((key, index) => key !== expected[index])) {
      return undefined;
    }
    const values: Record<string, unknown> = {};
    for (let index = 0; index < expected.length; index += 1) {
      const key = expected[index];
      if (!key) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      values[key] = descriptor.value;
    }
    return values['version'] === 1 &&
      typeof values['enabled'] === 'boolean' &&
      typeof values['clickGuard'] === 'boolean' &&
      typeof values['renderGuard'] === 'boolean' &&
      (values['enabled'] || (!values['clickGuard'] && !values['renderGuard']))
      ? (candidate as Readonly<CreativeBootV1>)
      : undefined;
  } catch {
    return undefined;
  }
}

function readRuntimeCapability(
  interfaces: Readonly<Record<string, unknown>>
): RuntimeCapabilityV1 | undefined {
  try {
    const descriptor = Object.getOwnPropertyDescriptor(interfaces, 'runtime.v1');
    if (!descriptor || !('value' in descriptor)) return undefined;
    const candidate = descriptor.value;
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      !Object.isFrozen(candidate) ||
      !(candidate as RuntimeCapabilityV1).document
    ) {
      return undefined;
    }
    return candidate as RuntimeCapabilityV1;
  } catch {
    return undefined;
  }
}

/** Build the inert, release-bound creative module for the coordinated runtime. */
export function createCreativeIntegrationRegistration(releaseId: string): IntegrationRegistration {
  return Object.freeze({
    abi: 1,
    id: CREATIVE_INTEGRATION_ID,
    phase: 'critical',
    releaseId,
    prepare: ({ config, interfaces }: IntegrationPrepareContext) => {
      const creative = readCreativeBoot(config);
      if (!creative) throw new TypeError('Creative boot configuration is invalid');
      const runtimeCapability = readRuntimeCapability(interfaces);
      const runtimeDocument = runtimeCapability?.document;
      if (!runtimeDocument) throw new TypeError('Creative runtime capability is unavailable');
      if (!creative.enabled || (!creative.clickGuard && !creative.renderGuard)) {
        return Object.freeze({ activate: () => undefined });
      }
      const runtime = createCreativeStartup({
        document: runtimeDocument,
        installClickGuard: () => installClickGuard(false),
        installDynamicIframeProxy: () => installDynamicIframeProxy(false),
        installDynamicImageProxy: () => installDynamicImageProxy(false),
      });

      return Object.freeze({
        activate: ({ afterCommit, onDispose }: IntegrationActivationContext) => {
          const runtimeRelease: { value?: () => void } = {};
          onDispose(() => runtimeRelease.value?.());
          const releaseRuntime = runtime.activate(creative);
          if (typeof releaseRuntime !== 'function') {
            throw new TypeError('Creative integration activation disposer is unavailable');
          }
          runtimeRelease.value = releaseRuntime;
          afterCommit(() => runtime.start(creative));
        },
      });
    },
  });
}
