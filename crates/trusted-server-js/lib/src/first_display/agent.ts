import type { BootstrapController } from '../core/bootstrap_controller';
import type { BootFailureReason } from '../kernel/fallback';

const HASH = /^[0-9a-f]{64}$/;
const MAX_OUTCOMES = 256;
const MAX_U32 = 4_294_967_295;
const ACTION_KINDS = new Set(['gpt_adm', 'aps']);
const OUTCOME_KINDS = new Set(['no_bid', 'failed', 'cancelled', ...ACTION_KINDS]);

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
    if (!this.options.bootstrap.startAction()) {
      this.stateValue = 'failed';
      return false;
    }
    this.mark('tsjs:first-display');
    try {
      this.options.driver.start(Object.freeze(actions), (slotId, result) => {
        this.settle(slotId, result);
      });
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
    if (this.stateValue !== 'active' || !this.pending.delete(slotId)) return;
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
