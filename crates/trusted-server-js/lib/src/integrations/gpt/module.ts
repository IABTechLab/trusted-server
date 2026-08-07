import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';
import {
  createSlotOperation,
  type RenderAttempt,
  type SlotOperationCreationResult,
  type SlotOperationOptions,
} from '../../services/render';
import type { PucBridge, PucGamAttemptInput } from '../../services/puc_bridge';
import type { SlotRequestOutcome, SlotService } from '../../services/slots';

import { installGptGuard, resetGuardState } from './script_guard';

export const GPT_INTEGRATION_ID = 'gpt' as const;

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

interface GptIntegrationRuntime {
  readonly start: (config: unknown) => void;
}

export interface GptSlotOperationInput extends Omit<PucGamAttemptInput, 'attempt'> {
  readonly attempt: RenderAttempt;
  readonly createFallback?: SlotOperationOptions['createFallback'];
  readonly operation: 'display' | 'refresh';
  readonly pucBridge: Pick<PucBridge, 'recordNonemptyGam' | 'registerGamAttempt'>;
  readonly requestClass: string;
  readonly slots: Pick<SlotService, 'request'>;
}

function settleFromSlotOutcome(
  attempt: RenderAttempt,
  bridge: GptSlotOperationInput['pucBridge'],
  bridgeInput: PucGamAttemptInput,
  outcome: SlotRequestOutcome
): void {
  try {
    if (outcome.status === 'empty') {
      attempt.fail('gam_empty');
      return;
    }
    if (outcome.status === 'rendered') {
      if (!bridge.recordNonemptyGam(bridgeInput)) attempt.fail('cycle_unattributable');
      return;
    }
    if (outcome.status === 'failed') {
      attempt.fail(outcome.reason);
      return;
    }
    if (outcome.status === 'cancelled') attempt.cancel(outcome.reason);
  } catch {
    try {
      attempt.fail('internal_error');
    } catch {
      // The attempt latch remains the terminal authority.
    }
  }
}

/**
 * Join one TS-owned physical GPT cycle to its primary render attempt.
 *
 * Only the slot service may identify an attributable empty cycle. The resulting
 * `gam_empty` transition is therefore the sole path that can activate the
 * optional `SlotOperation` fallback child.
 */
export function startGptSlotOperation(input: GptSlotOperationInput): SlotOperationCreationResult {
  const operation = createSlotOperation({
    primary: input.attempt,
    ...(input.createFallback === undefined ? {} : { createFallback: input.createFallback }),
  });
  if (!operation.ok) return operation;

  const bridgeInput = Object.freeze({
    artifact: input.artifact,
    attempt: input.attempt,
    owner: input.owner,
    reservationId: input.reservationId,
  });
  const registered = (() => {
    try {
      return input.pucBridge.registerGamAttempt(bridgeInput);
    } catch {
      return false;
    }
  })();
  if (!registered) {
    try {
      input.attempt.fail('gpt_request_failed');
    } catch {
      // The operation still observes any terminal result already committed by the bridge.
    }
    return operation;
  }

  let handle: ReturnType<SlotService['request']>;
  try {
    handle = input.slots.request({
      intentId: input.attempt.id,
      navigationGeneration: input.attempt.navigationGeneration,
      operation: input.operation,
      registeredSlotId: input.attempt.slot,
      requestClass: input.requestClass,
    });
  } catch {
    input.attempt.fail('gpt_request_failed');
    return operation;
  }

  let handleDisposed = false;
  const disposeHandle = (): void => {
    if (handleDisposed) return;
    handleDisposed = true;
    try {
      handle.dispose();
    } catch {
      // Attempt settlement remains authoritative when request cleanup throws.
    }
  };
  const observing = (() => {
    try {
      return input.attempt.onSettled(disposeHandle);
    } catch {
      return false;
    }
  })();
  if (!observing) {
    disposeHandle();
    try {
      input.attempt.fail('internal_error');
    } catch {
      // A concurrently terminal attempt cannot be overwritten.
    }
    return operation;
  }

  void handle.result.then(
    (outcome) => settleFromSlotOutcome(input.attempt, input.pucBridge, bridgeInput, outcome),
    () => {
      try {
        input.attempt.fail('gpt_request_failed');
      } catch {
        // A late rejected request cannot overwrite an existing terminal outcome.
      }
    }
  );
  return operation;
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

    for (const name of names) {
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

function readGptRuntime(
  interfaces: Readonly<Record<string, unknown>>
): GptIntegrationRuntime | undefined {
  const descriptor = Object.getOwnPropertyDescriptor(interfaces, GPT_INTEGRATION_ID);
  if (!descriptor || !('value' in descriptor)) return undefined;
  const candidate = descriptor.value;
  if (
    typeof candidate !== 'object' ||
    candidate === null ||
    Array.isArray(candidate) ||
    !Object.isFrozen(candidate) ||
    Reflect.ownKeys(candidate).length !== 1
  ) {
    return undefined;
  }
  const start = Object.getOwnPropertyDescriptor(candidate, 'start');
  if (!start || !('value' in start) || typeof start.value !== 'function') return undefined;
  return candidate as GptIntegrationRuntime;
}

/** Build the release-bound GPT module registered by the coordinated runtime. */
export function createGptIntegrationRegistration(release: string): IntegrationRegistration {
  return Object.freeze({
    id: GPT_INTEGRATION_ID,
    release,
    prepare: ({ config, interfaces }: IntegrationPrepareContext) => {
      if (!validFrozenConfig(config)) throw new TypeError('GPT integration config is invalid');
      const runtime = readGptRuntime(interfaces);
      if (!runtime) throw new TypeError('GPT integration runtime is unavailable');

      return Object.freeze({
        activate: ({ afterCommit, onDispose }: IntegrationActivationContext) => {
          // Register restoration before the first live browser mutation.
          onDispose(resetGuardState);
          installGptGuard();
          afterCommit(() => runtime.start(config));
        },
      });
    },
  });
}
