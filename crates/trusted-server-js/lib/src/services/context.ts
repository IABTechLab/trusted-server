import type { DisposeCallback } from '../kernel/disposable';

const MAX_MANIFEST_INTEGRATIONS = 16;
const INTEGRATION_ID = /^[a-z0-9][a-z0-9_-]{0,63}$/;

/** Owner boundary required for generation-scoped contributor registration. */
export interface ContextContributorOwner {
  readonly generation: object;
  readonly isCurrent: () => boolean;
  readonly onDispose: (kind: string, callback: DisposeCallback) => void;
}

/** One integration's document-scoped contribution to an auction request. */
export type AuctionContextContributor = () => Readonly<Record<string, unknown>> | undefined;

/** Sanitized observation emitted when one contributor cannot be copied. */
export interface AuctionContextContributorFailure {
  readonly integrationId: string;
  readonly reason: 'contributor_failed';
}

/** Runtime-owned contributor registry options. */
export interface AuctionContextRegistryOptions {
  readonly manifestIntegrationIds: readonly string[];
  readonly runtimeOwner: ContextContributorOwner;
  readonly onContributorFailure?: (failure: AuctionContextContributorFailure) => void;
}

/** Frozen test-only inventory for the runtime-owned registry. */
export interface AuctionContextRegistryInventory {
  readonly disposed: boolean;
  readonly registrations: readonly string[];
}

/** Runtime-owned registry that snapshots contributors in manifest order. */
export interface AuctionContextRegistry {
  readonly register: (
    integrationId: string,
    contributor: AuctionContextContributor,
    owner: ContextContributorOwner
  ) => boolean;
  readonly snapshot: () => Readonly<Record<string, unknown>>;
  readonly dispose: () => void;
  readonly snapshotInventoryForTest: () => AuctionContextRegistryInventory;
}

interface ContributorRecord {
  readonly contributor: AuctionContextContributor;
  readonly owner: ContextContributorOwner;
  readonly ownerGeneration: object;
}

const INVALID = Symbol('invalid_context_value');

function snapshotManifest(candidate: readonly string[]): readonly string[] {
  if (!Object.isFrozen(candidate) || candidate.length > MAX_MANIFEST_INTEGRATIONS) {
    throw new TypeError('Auction context manifest must be frozen and bounded');
  }
  const seen = new Set<string>();
  const snapshot: string[] = [];
  for (const id of candidate) {
    if (typeof id !== 'string' || !INTEGRATION_ID.test(id) || seen.has(id)) {
      throw new TypeError('Auction context manifest contains an invalid integration id');
    }
    seen.add(id);
    snapshot.push(id);
  }
  return Object.freeze(snapshot);
}

function cloneContextValue(value: unknown, ancestors: Set<object>): unknown | typeof INVALID {
  if (
    value === null ||
    typeof value === 'string' ||
    typeof value === 'boolean' ||
    (typeof value === 'number' && Number.isFinite(value))
  ) {
    return value;
  }
  if (typeof value !== 'object') return INVALID;
  if (ancestors.has(value)) return INVALID;

  try {
    const prototype = Object.getPrototypeOf(value) as unknown;
    const array = Array.isArray(value);
    if ((array && prototype !== Array.prototype) || (!array && prototype !== Object.prototype)) {
      return INVALID;
    }
    if (Object.getOwnPropertySymbols(value).length > 0) return INVALID;
    ancestors.add(value);
    if (array) {
      const names = Object.getOwnPropertyNames(value);
      if (names.length !== value.length + 1 || !names.includes('length')) return INVALID;
      const output: unknown[] = [];
      for (let index = 0; index < value.length; index += 1) {
        const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
        if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return INVALID;
        const cloned = cloneContextValue(descriptor.value, ancestors);
        if (cloned === INVALID) return INVALID;
        output.push(cloned);
      }
      return Object.freeze(output);
    }

    const output: Record<string, unknown> = {};
    for (const key of Object.getOwnPropertyNames(value)) {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return INVALID;
      const cloned = cloneContextValue(descriptor.value, ancestors);
      if (cloned === INVALID) return INVALID;
      Object.defineProperty(output, key, {
        configurable: true,
        enumerable: true,
        value: cloned,
        writable: true,
      });
    }
    return Object.freeze(output);
  } catch {
    return INVALID;
  } finally {
    ancestors.delete(value);
  }
}

function copyContribution(
  value: Readonly<Record<string, unknown>>
): Readonly<Record<string, unknown>> | undefined {
  const cloned = cloneContextValue(value, new Set<object>());
  return cloned === INVALID ||
    typeof cloned !== 'object' ||
    cloned === null ||
    Array.isArray(cloned)
    ? undefined
    : (cloned as Readonly<Record<string, unknown>>);
}

class AuctionContextRegistryOwner implements AuctionContextRegistry {
  private readonly manifestIntegrationIds: readonly string[];
  private readonly manifestSet: ReadonlySet<string>;
  private readonly runtimeGeneration: object;
  private readonly registrations = new Map<string, ContributorRecord>();
  private readonly runtimeOwner: ContextContributorOwner;
  private readonly onContributorFailure:
    ((failure: AuctionContextContributorFailure) => void) | undefined;
  private isDisposed = false;

  public constructor(options: AuctionContextRegistryOptions) {
    this.manifestIntegrationIds = snapshotManifest(options.manifestIntegrationIds);
    this.manifestSet = new Set(this.manifestIntegrationIds);
    this.runtimeOwner = options.runtimeOwner;
    this.runtimeGeneration = options.runtimeOwner.generation;
    this.onContributorFailure = options.onContributorFailure;
    try {
      options.runtimeOwner.onDispose('auction-context-registry', () => this.dispose());
    } catch {
      this.dispose();
    }
    if (!this.runtimeIsCurrent()) this.dispose();
  }

  public register(
    integrationId: string,
    contributor: AuctionContextContributor,
    owner: ContextContributorOwner
  ): boolean {
    if (
      !this.runtimeIsCurrent() ||
      !this.manifestSet.has(integrationId) ||
      this.registrations.has(integrationId) ||
      typeof contributor !== 'function' ||
      !this.ownerIsCurrent(owner, owner.generation)
    ) {
      return false;
    }
    const record: ContributorRecord = Object.freeze({
      contributor,
      owner,
      ownerGeneration: owner.generation,
    });
    this.registrations.set(integrationId, record);
    try {
      owner.onDispose('auction-context-contributor', () => {
        if (this.registrations.get(integrationId) === record) {
          this.registrations.delete(integrationId);
        }
      });
    } catch {
      this.registrations.delete(integrationId);
      return false;
    }
    if (!this.runtimeIsCurrent() || !this.ownerIsCurrent(owner, record.ownerGeneration)) {
      this.registrations.delete(integrationId);
      return false;
    }
    return true;
  }

  public snapshot(): Readonly<Record<string, unknown>> {
    if (!this.runtimeIsCurrent()) {
      this.dispose();
      return Object.freeze({});
    }
    const output: Record<string, unknown> = {};
    for (const integrationId of this.manifestIntegrationIds) {
      const record = this.registrations.get(integrationId);
      if (!record || !this.ownerIsCurrent(record.owner, record.ownerGeneration)) continue;
      let contribution: Readonly<Record<string, unknown>> | undefined;
      try {
        const candidate = record.contributor();
        if (candidate === undefined) continue;
        contribution = copyContribution(candidate);
      } catch {
        contribution = undefined;
      }
      if (!contribution) {
        this.reportFailure(integrationId);
        continue;
      }
      if (!this.runtimeIsCurrent()) return Object.freeze({});
      if (!this.ownerIsCurrent(record.owner, record.ownerGeneration)) continue;
      for (const [key, value] of Object.entries(contribution)) {
        Object.defineProperty(output, key, {
          configurable: true,
          enumerable: true,
          value,
          writable: true,
        });
      }
    }
    return this.runtimeIsCurrent() ? Object.freeze(output) : Object.freeze({});
  }

  public dispose(): void {
    if (this.isDisposed) return;
    this.isDisposed = true;
    this.registrations.clear();
  }

  public snapshotInventoryForTest(): AuctionContextRegistryInventory {
    return Object.freeze({
      disposed: this.isDisposed,
      registrations: Object.freeze(
        this.manifestIntegrationIds.filter((id) => this.registrations.has(id))
      ),
    });
  }

  private runtimeIsCurrent(): boolean {
    return (
      !this.isDisposed &&
      this.runtimeOwner.generation === this.runtimeGeneration &&
      this.ownerIsCurrent(this.runtimeOwner, this.runtimeGeneration)
    );
  }

  private ownerIsCurrent(owner: ContextContributorOwner, generation: object): boolean {
    try {
      return owner.generation === generation && owner.isCurrent();
    } catch {
      return false;
    }
  }

  private reportFailure(integrationId: string): void {
    try {
      this.onContributorFailure?.(Object.freeze({ integrationId, reason: 'contributor_failed' }));
    } catch {
      // Observational reporting cannot affect the remaining manifest contributors.
    }
  }
}

/** Construct one manifest-bounded, runtime-owned context contributor registry. */
export function createAuctionContextRegistry(
  options: AuctionContextRegistryOptions
): AuctionContextRegistry {
  return new AuctionContextRegistryOwner(options);
}
