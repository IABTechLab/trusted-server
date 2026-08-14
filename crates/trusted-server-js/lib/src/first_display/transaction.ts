import type { FirstDisplaySliceId } from '../kernel/release_catalog';

const HASH = /^[0-9a-f]{64}$/;
const FIRST_DISPLAY_SRC =
  /^\/static\/tsjs=tsjs-first-display\.min\.js\?m=[0-9a-f]{4}&v=[0-9a-f]{64}$/;

export type FirstDisplayTransactionState =
  'collecting' | 'preparing' | 'activating' | 'active' | 'failed' | 'disposed';

export interface FirstDisplaySlicePrepareContext {
  readonly releaseId: string;
  readonly generation: number;
  readonly sliceId: FirstDisplaySliceId;
}

export interface FirstDisplaySliceActivationContext {
  readonly own: (dispose: () => void) => void;
}

export interface PreparedFirstDisplaySliceV1 {
  readonly activate: (context: FirstDisplaySliceActivationContext) => void;
}

export interface FirstDisplaySliceRegistrationV1 {
  readonly abi: 1;
  readonly id: FirstDisplaySliceId;
  readonly releaseId: string;
  readonly generation: number;
  readonly order: number;
  readonly prepare: (context: FirstDisplaySlicePrepareContext) => PreparedFirstDisplaySliceV1;
}

export interface FirstDisplayTransactionOptions {
  readonly document: Document;
  readonly script: HTMLScriptElement;
  readonly releaseId: string;
  readonly generation: number;
  readonly expectedSliceIds: readonly FirstDisplaySliceId[];
  readonly isCurrentGeneration: () => boolean;
  readonly onDisposalError?: (error: unknown) => void;
}

export interface FirstDisplayTransaction {
  readonly state: FirstDisplayTransactionState;
  readonly register: (candidate: unknown) => boolean;
  readonly activate: () => boolean;
  readonly dispose: () => void;
}

function exactRecord(value: unknown, keys: readonly string[]): Record<string, unknown> | undefined {
  if (
    typeof value !== 'object' ||
    value === null ||
    Array.isArray(value) ||
    Object.getPrototypeOf(value) !== Object.prototype
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

function snapshotRegistration(candidate: unknown): FirstDisplaySliceRegistrationV1 | undefined {
  try {
    const fields = exactRecord(candidate, [
      'abi',
      'id',
      'releaseId',
      'generation',
      'order',
      'prepare',
    ]);
    if (
      !fields ||
      fields.abi !== 1 ||
      typeof fields.id !== 'string' ||
      typeof fields.releaseId !== 'string' ||
      !HASH.test(fields.releaseId) ||
      !Number.isInteger(fields.generation) ||
      (fields.generation as number) < 1 ||
      (fields.generation as number) > 4_294_967_295 ||
      !Number.isInteger(fields.order) ||
      (fields.order as number) < 1 ||
      typeof fields.prepare !== 'function'
    ) {
      return undefined;
    }
    return Object.freeze({
      abi: 1,
      id: fields.id as FirstDisplaySliceId,
      releaseId: fields.releaseId,
      generation: fields.generation as number,
      order: fields.order as number,
      prepare: fields.prepare as FirstDisplaySliceRegistrationV1['prepare'],
    });
  } catch {
    return undefined;
  }
}

class FirstDisplayTransactionOwner implements FirstDisplayTransaction {
  private stateValue: FirstDisplayTransactionState = 'collecting';
  private readonly registrations: FirstDisplaySliceRegistrationV1[] = [];
  private readonly disposers: Array<() => void> = [];

  public constructor(private readonly options: FirstDisplayTransactionOptions) {}

  public get state(): FirstDisplayTransactionState {
    return this.stateValue;
  }

  public register(candidate: unknown): boolean {
    if (this.stateValue !== 'collecting' || !this.authenticated()) return this.reject();
    const registration = snapshotRegistration(candidate);
    const expectedIndex = this.registrations.length;
    if (
      !registration ||
      registration.releaseId !== this.options.releaseId ||
      registration.generation !== this.options.generation ||
      registration.order !== expectedIndex + 1 ||
      registration.id !== this.options.expectedSliceIds[expectedIndex]
    ) {
      return this.reject();
    }
    this.registrations.push(registration);
    return true;
  }

  public activate(): boolean {
    if (
      this.stateValue !== 'collecting' ||
      this.registrations.length !== this.options.expectedSliceIds.length ||
      !this.authenticated()
    ) {
      return this.reject();
    }
    const prepared: PreparedFirstDisplaySliceV1[] = [];
    this.stateValue = 'preparing';
    try {
      for (const registration of this.registrations) {
        const candidate = registration.prepare(
          Object.freeze({
            releaseId: this.options.releaseId,
            generation: this.options.generation,
            sliceId: registration.id,
          })
        );
        const fields = exactRecord(candidate, ['activate']);
        if (!fields || typeof fields.activate !== 'function')
          throw new TypeError('invalid prepared slice');
        prepared.push(
          Object.freeze({ activate: fields.activate as PreparedFirstDisplaySliceV1['activate'] })
        );
      }
      if (!this.authenticated()) throw new TypeError('stale first-display owner');
      this.stateValue = 'activating';
      let ownershipOpen = true;
      const context = Object.freeze({
        own: (dispose: () => void): void => {
          if (!ownershipOpen || typeof dispose !== 'function') {
            throw new TypeError('first-display disposer registration is closed');
          }
          this.disposers.push(dispose);
        },
      });
      for (const slice of prepared) slice.activate(context);
      ownershipOpen = false;
      if (!this.authenticated()) throw new TypeError('stale first-display activation');
      this.stateValue = 'active';
      return true;
    } catch {
      this.stateValue = 'failed';
      this.unwind();
      return false;
    }
  }

  public dispose(): void {
    if (this.stateValue === 'disposed') return;
    this.stateValue = 'disposed';
    this.unwind();
    this.registrations.length = 0;
  }

  private authenticated(): boolean {
    try {
      const { document, script, releaseId, generation, isCurrentGeneration } = this.options;
      const origin = document.defaultView?.location.origin;
      if (!origin || !HASH.test(releaseId) || generation < 1 || !isCurrentGeneration())
        return false;
      const source = new URL(script.src, origin);
      return (
        script.id === 'trustedserver-js' &&
        script.isConnected &&
        document.currentScript === script &&
        source.origin === origin &&
        source.hash === '' &&
        FIRST_DISPLAY_SRC.test(`${source.pathname}${source.search}`)
      );
    } catch {
      return false;
    }
  }

  private reject(): false {
    if (this.stateValue !== 'active' && this.stateValue !== 'disposed') {
      this.stateValue = 'failed';
      this.unwind();
    }
    return false;
  }

  private unwind(): void {
    for (let index = this.disposers.length - 1; index >= 0; index -= 1) {
      try {
        this.disposers[index]?.();
      } catch (error) {
        try {
          this.options.onDisposalError?.(error);
        } catch {
          // Continue releasing every independent provisional effect.
        }
      }
    }
    this.disposers.length = 0;
  }
}

/** Create the closure-private collector for exactly one authenticated agent artifact. */
export function createFirstDisplayTransaction(
  options: FirstDisplayTransactionOptions
): FirstDisplayTransaction {
  const owner = new FirstDisplayTransactionOwner(options);
  return Object.freeze({
    get state() {
      return owner.state;
    },
    register: (candidate: unknown) => owner.register(candidate),
    activate: () => owner.activate(),
    dispose: () => owner.dispose(),
  });
}
