import type { BootstrapController } from '../core/bootstrap_controller';
import type { BootFailureReason } from '../kernel/fallback';

import {
  firstDisplayComponentRegistration,
  registerCurrentFirstDisplayComponent,
} from './registration';
import type {
  FirstDisplaySliceActivationContext,
} from './transaction';
import type {
  FirstDisplaySliceHost,
  InitialSliceInstaller,
  OptionalFirstDisplaySliceId,
} from './slices/definition';

const HASH = /^[0-9a-f]{64}$/;
const MAX_OUTCOMES = 256;
const MAX_U32 = 4_294_967_295;
const ACTION_KINDS = new Set(['gpt_adm', 'aps']);
const OUTCOME_KINDS = new Set(['no_bid', 'failed', 'cancelled', ...ACTION_KINDS]);
const TERMINAL_RESULTS = new Set(['accepted', 'failed', 'cancelled']);

export type FirstDisplayProjectedKind = 'no_bid' | 'failed' | 'cancelled' | 'gpt_adm' | 'aps';
export type FirstDisplayTerminalResult = 'accepted' | 'failed' | 'cancelled';
export type FirstDisplayAgentState =
  'ready' | 'active' | 'terminal' | 'painted' | 'failed' | 'disposed';

export interface FirstDisplayBatchOutcomeV1 {
  readonly slotId: string;
  readonly kind: FirstDisplayProjectedKind;
}

export interface FirstDisplayBatchV1 {
  readonly version: 1;
  readonly projectionDigest: string;
  readonly outcomes: readonly FirstDisplayBatchOutcomeV1[];
}

export interface FirstDisplayDriver {
  readonly start: (
    outcomes: readonly FirstDisplayBatchOutcomeV1[],
    onFirstAction: () => boolean,
    onTerminal: (slotId: string, result: FirstDisplayTerminalResult) => void
  ) => void;
  readonly sealTsAdmission: () => void;
  readonly dispose: () => void;
}

export interface FirstDisplayPaintScheduler {
  readonly hidden: () => boolean;
  readonly requestFrame: (callback: () => void) => void;
  readonly scheduleHidden: (callback: () => void) => void;
}

export interface FirstDisplayAgentOptions {
  readonly batch: unknown;
  readonly bootstrap: BootstrapController;
  readonly driver: FirstDisplayDriver;
  readonly performance: Readonly<{ mark: (name: string) => void }>;
  readonly paint: FirstDisplayPaintScheduler;
  readonly onProtectedPaint: () => void;
  readonly onFailure: (reason: BootFailureReason) => void;
  readonly onPrebidAdmissionFailure?: (reason: 'prebid_admission_failed') => void;
  readonly initialMutationRevision?: number;
}

/** Bootstrap-owned dependencies supplied only to the release-bound base component. */
export interface FirstDisplayAgentRegistrationHostV1 {
  readonly options: FirstDisplayAgentOptions;
  readonly sliceBindings: (id: string) => unknown;
}

interface PreparedFirstDisplayBaseV1 {
  readonly activate: (context: FirstDisplaySliceActivationContext) => void;
  readonly sliceHost: FirstDisplaySliceHost;
}

export interface FirstDisplayAgentSnapshotV1 {
  readonly state: FirstDisplayAgentState;
  readonly mutationRevision: number;
  readonly initialDisplayCommitted: boolean;
  readonly outcomes: readonly Readonly<{
    slotId: string;
    result: FirstDisplayTerminalResult | 'no_bid' | 'failed' | 'cancelled';
  }>[];
}

export interface FirstDisplayAgent {
  readonly state: FirstDisplayAgentState;
  readonly start: () => boolean;
  readonly admitTsBid: (complete: () => void) => boolean;
  readonly observeNativeMutation: () => boolean;
  readonly snapshot: () => FirstDisplayAgentSnapshotV1;
  readonly dispose: () => void;
}

function exactRecord(value: unknown, keys: readonly string[]): Record<string, unknown> | undefined {
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
}

function snapshotBatch(value: unknown): FirstDisplayBatchV1 | undefined {
  try {
    const fields = exactRecord(value, ['version', 'projectionDigest', 'outcomes']);
    if (
      !fields ||
      fields.version !== 1 ||
      typeof fields.projectionDigest !== 'string' ||
      !HASH.test(fields.projectionDigest) ||
      !Array.isArray(fields.outcomes) ||
      !Object.isFrozen(fields.outcomes) ||
      fields.outcomes.length === 0 ||
      fields.outcomes.length > MAX_OUTCOMES
    ) {
      return undefined;
    }
    const seen = new Set<string>();
    const outcomes: FirstDisplayBatchOutcomeV1[] = [];
    for (const candidate of fields.outcomes) {
      const outcome = exactRecord(candidate, ['slotId', 'kind']);
      if (
        !outcome ||
        typeof outcome.slotId !== 'string' ||
        outcome.slotId.length === 0 ||
        outcome.slotId.length > 1024 ||
        seen.has(outcome.slotId) ||
        typeof outcome.kind !== 'string' ||
        !OUTCOME_KINDS.has(outcome.kind)
      ) {
        return undefined;
      }
      seen.add(outcome.slotId);
      outcomes.push(
        Object.freeze({
          slotId: outcome.slotId,
          kind: outcome.kind as FirstDisplayProjectedKind,
        })
      );
    }
    return Object.freeze({
      version: 1,
      projectionDigest: fields.projectionDigest,
      outcomes: Object.freeze(outcomes),
    });
  } catch {
    return undefined;
  }
}

class FirstDisplayAgentOwner implements FirstDisplayAgent {
  private readonly batch: FirstDisplayBatchV1 | undefined;
  private readonly results = new Map<
    string,
    FirstDisplayTerminalResult | 'no_bid' | 'failed' | 'cancelled'
  >();
  private readonly pending = new Set<string>();
  private stateValue: FirstDisplayAgentState = 'ready';
  private mutationRevision: number;
  private initialDisplayCommitted = false;
  private sealed = false;
  private disposedDriver = false;
  private failed = false;
  private actionStarted = false;

  public constructor(private readonly options: FirstDisplayAgentOptions) {
    this.batch = snapshotBatch(options.batch);
    this.mutationRevision = options.initialMutationRevision ?? 0;
  }

  public get state(): FirstDisplayAgentState {
    return this.stateValue;
  }

  public start(): boolean {
    if (this.stateValue !== 'ready') return false;
    if (!this.batch) return this.fail('abi_mismatch');
    if (!this.options.bootstrap.registerAgent()) {
      this.stateValue = 'failed';
      return false;
    }

    const actions: FirstDisplayBatchOutcomeV1[] = [];
    for (const outcome of this.batch.outcomes) {
      if (ACTION_KINDS.has(outcome.kind)) {
        this.pending.add(outcome.slotId);
        actions.push(outcome);
      } else {
        this.results.set(outcome.slotId, outcome.kind as 'no_bid' | 'failed' | 'cancelled');
      }
    }
    this.stateValue = 'active';
    if (actions.length === 0) {
      this.recordTerminal();
      return true;
    }
    try {
      this.options.driver.start(
        Object.freeze(actions),
        () => this.recordFirstAction(),
        (slotId, result) => {
          this.settle(slotId, result);
        }
      );
      return true;
    } catch {
      return this.fail('bundle_partial');
    }
  }

  public admitTsBid(complete: () => void): boolean {
    if (!this.sealed || typeof complete !== 'function') return false;
    try {
      complete();
    } catch {
      // Bidder completion isolation cannot reopen sealed TS admission.
    }
    try {
      this.options.onPrebidAdmissionFailure?.('prebid_admission_failed');
    } catch {
      // Diagnostics cannot alter the terminal no-bid completion.
    }
    return false;
  }

  public observeNativeMutation(): boolean {
    if (this.stateValue !== 'painted' || this.failed) return false;
    if (this.mutationRevision >= MAX_U32) return this.fail('bundle_partial');
    this.mutationRevision += 1;
    return true;
  }

  public snapshot(): FirstDisplayAgentSnapshotV1 {
    const outcomes = this.batch?.outcomes.map(({ slotId }) =>
      Object.freeze({
        slotId,
        result: this.results.get(slotId) ?? 'failed',
      })
    );
    return Object.freeze({
      state: this.stateValue,
      mutationRevision: this.mutationRevision,
      initialDisplayCommitted: this.initialDisplayCommitted,
      outcomes: Object.freeze(outcomes ?? []),
    });
  }

  public dispose(): void {
    if (this.stateValue === 'disposed') return;
    this.stateValue = 'disposed';
    this.disposeDriver();
    this.pending.clear();
  }

  private settle(slotId: string, result: FirstDisplayTerminalResult): void {
    if (!this.actionStarted) {
      this.fail('bundle_partial');
      return;
    }
    if (this.stateValue !== 'active') return;
    if (typeof result !== 'string' || !TERMINAL_RESULTS.has(result)) {
      this.fail('bundle_partial');
      return;
    }
    if (!this.pending.delete(slotId)) {
      if (!this.results.has(slotId)) this.fail('bundle_partial');
      return;
    }
    this.results.set(slotId, result);
    if (result === 'accepted') this.initialDisplayCommitted = true;
    if (this.pending.size === 0) this.recordTerminal();
  }

  private recordTerminal(): void {
    if (this.stateValue !== 'active' || this.pending.size !== 0) return;
    this.stateValue = 'terminal';
    this.options.bootstrap.settle();
    this.mark('tsjs:first-display-terminal');
    this.scheduleProtectedPaint(2);
  }

  private recordFirstAction(): boolean {
    if (this.stateValue !== 'active' || this.actionStarted) return this.fail('bundle_partial');
    if (!this.options.bootstrap.startAction()) {
      if (this.options.bootstrap.state !== 'failed') return this.fail('bundle_partial');
      this.failed = true;
      this.stateValue = 'failed';
      this.disposeDriver();
      this.pending.clear();
      return false;
    }
    this.actionStarted = true;
    this.mark('tsjs:first-display');
    return true;
  }

  private scheduleProtectedPaint(remaining: number): void {
    const next = (): void => {
      if (this.stateValue !== 'terminal') return;
      if (remaining > 1) {
        this.scheduleProtectedPaint(remaining - 1);
        return;
      }
      try {
        this.options.driver.sealTsAdmission();
      } catch {
        this.fail('bundle_partial');
        return;
      }
      this.sealed = true;
      this.stateValue = 'painted';
      this.mark('tsjs:first-display-paint');
      try {
        this.options.onProtectedPaint();
      } catch {
        this.fail('bundle_partial');
      }
    };
    try {
      if (this.options.paint.hidden()) this.options.paint.scheduleHidden(next);
      else this.options.paint.requestFrame(next);
    } catch {
      this.fail('bundle_partial');
    }
  }

  private mark(name: string): void {
    try {
      this.options.performance.mark(name);
    } catch {
      // Timing observability cannot alter display ownership.
    }
  }

  private fail(reason: BootFailureReason): false {
    if (this.failed || this.stateValue === 'disposed') return false;
    this.failed = true;
    this.stateValue = 'failed';
    if (!this.options.bootstrap.fail(reason)) {
      try {
        this.options.onFailure(reason);
      } catch {
        // Post-paint failure publication cannot restore provisional authority.
      }
    }
    this.disposeDriver();
    this.pending.clear();
    return false;
  }

  private disposeDriver(): void {
    if (this.disposedDriver) return;
    this.disposedDriver = true;
    try {
      this.options.driver.dispose();
    } catch {
      // Disposal is generation-latched; physical cleanup failure cannot restore authority.
    }
  }
}

/** Create the bounded provisional lifecycle owner; it publishes no runtime API. */
export function createFirstDisplayAgent(options: FirstDisplayAgentOptions): FirstDisplayAgent {
  const owner = new FirstDisplayAgentOwner(options);
  return Object.freeze({
    get state() {
      return owner.state;
    },
    start: () => owner.start(),
    admitTsBid: (complete: () => void) => owner.admitTsBid(complete),
    observeNativeMutation: () => owner.observeNativeMutation(),
    snapshot: () => owner.snapshot(),
    dispose: () => owner.dispose(),
  });
}

function prepareRegisteredAgent(host: unknown): PreparedFirstDisplayBaseV1 {
  try {
    if (
      typeof host !== 'object' ||
      host === null ||
      Array.isArray(host) ||
      Object.getPrototypeOf(host) !== Object.prototype ||
      !Object.isFrozen(host) ||
      Reflect.ownKeys(host).length !== 2
    ) {
      throw new TypeError('invalid first-display base host');
    }
    const descriptor = Object.getOwnPropertyDescriptor(host, 'options');
    const bindingsDescriptor = Object.getOwnPropertyDescriptor(host, 'sliceBindings');
    if (
      !descriptor?.enumerable ||
      !('value' in descriptor) ||
      typeof descriptor.value !== 'object' ||
      descriptor.value === null ||
      !Object.isFrozen(descriptor.value) ||
      !bindingsDescriptor?.enumerable ||
      !('value' in bindingsDescriptor) ||
      typeof bindingsDescriptor.value !== 'function'
    ) {
      throw new TypeError('invalid first-display base options');
    }
    const options = descriptor.value as FirstDisplayAgentOptions;
    const sliceBindings = bindingsDescriptor.value as (id: string) => unknown;
    const sliceHost: FirstDisplaySliceHost = Object.freeze({
      activate: (
        id: OptionalFirstDisplaySliceId,
        own: FirstDisplaySliceActivationContext['own'],
        install?: InitialSliceInstaller
      ): void => {
        if (typeof install !== 'function') {
          throw new TypeError(`unimplemented first-display slice: ${id}`);
        }
        install(sliceBindings(id), own);
      },
    });
    return Object.freeze({
      activate: (context: FirstDisplaySliceActivationContext): void => {
        const agent = createFirstDisplayAgent(options);
        context.own(() => agent.dispose());
        context.afterActivate(() => {
          if (!agent.start()) throw new TypeError('first-display agent did not start');
        });
      },
      sliceHost,
    });
  } catch {
    throw new TypeError('invalid first-display base host');
  }
}

registerCurrentFirstDisplayComponent(
  firstDisplayComponentRegistration('first_display', 1, prepareRegisteredAgent)
);
