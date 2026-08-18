import type { DisposeCallback } from '../kernel/disposable';
import { MAX_MANIFEST_MODULES } from '../kernel/release_catalog';

const INTEGRATION_ID = /^[a-z0-9][a-z0-9_-]{0,63}$/;
// Context shares the existing /auction request-body ceiling. The structural
// bound follows from the smallest repeated JSON unit (`0,`), and the key bound
// reserves the seven bytes required by a one-property `{<key>:null}` object.
const MAX_CONTEXT_JSON_BYTES = 256 * 1024;
const MAX_CONTEXT_STRUCTURE_ENTRIES = Math.floor((MAX_CONTEXT_JSON_BYTES - 1) / 2);
const MAX_CONTEXT_ENCODED_KEY_BYTES = MAX_CONTEXT_JSON_BYTES - 7;

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

type JsonPrimitive = null | boolean | number | string;

interface ContextMeasurement {
  readonly jsonBytes: number;
  readonly keyBytes: number;
  readonly structureEntries: number;
}

interface PrimitiveSnapshot {
  readonly kind: 'primitive';
  readonly measurement: ContextMeasurement;
  readonly value: JsonPrimitive;
}

interface ContextSnapshotEntry {
  readonly encodedKeyBytes: number;
  readonly key: string;
  readonly sourceValue: unknown;
  snapshot: ContextSnapshotNode | PrimitiveSnapshot | undefined;
}

interface ContextSnapshotNode {
  readonly array: boolean;
  readonly entries: readonly ContextSnapshotEntry[];
  readonly kind: 'node';
  readonly source: object;
}

interface ContextSnapshotGraph {
  readonly nodes: readonly ContextSnapshotNode[];
  readonly root: ContextSnapshotNode;
}

interface ClonedContextValue {
  readonly measurement: ContextMeasurement;
  readonly value: unknown;
}

interface CopiedContributionEntry extends ContextMeasurement {
  readonly key: string;
  readonly value: unknown;
}

interface CopiedContribution {
  readonly entries: readonly CopiedContributionEntry[];
}

interface AcceptedContributionEntry {
  readonly contribution: CopiedContributionEntry;
  readonly integrationId: string;
  readonly record: ContributorRecord;
}

interface TraversalBudget {
  keyBytes: number;
  structureEntries: number;
}

interface TraversalFrame {
  readonly node: ContextSnapshotNode;
  index: number;
}

function snapshotManifest(candidate: readonly string[]): readonly string[] {
  if (!Object.isFrozen(candidate) || candidate.length > MAX_MANIFEST_MODULES) {
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

function boundedSum(left: number, right: number, maximum: number): number {
  return left > maximum - right ? maximum + 1 : left + right;
}

function encodedJsonStringBytes(value: string, maximum: number): number {
  let bytes = 2;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    let encodedBytes: number;
    if (code === 0x22 || code === 0x5c) encodedBytes = 2;
    else if (code <= 0x1f) {
      encodedBytes =
        code === 0x08 || code === 0x09 || code === 0x0a || code === 0x0c || code === 0x0d ? 2 : 6;
    } else if (code <= 0x7f) encodedBytes = 1;
    else if (code <= 0x7ff) encodedBytes = 2;
    else if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (next >= 0xdc00 && next <= 0xdfff) {
        encodedBytes = 4;
        index += 1;
      } else encodedBytes = 6;
    } else if (code >= 0xdc00 && code <= 0xdfff) encodedBytes = 6;
    else encodedBytes = 3;
    bytes = boundedSum(bytes, encodedBytes, maximum);
    if (bytes > maximum) return bytes;
  }
  return bytes;
}

function snapshotPrimitive(value: unknown): PrimitiveSnapshot | undefined {
  let jsonBytes: number;
  if (value === null) jsonBytes = 4;
  else if (typeof value === 'boolean') jsonBytes = value ? 4 : 5;
  else if (typeof value === 'number' && Number.isFinite(value)) jsonBytes = `${value}`.length;
  else if (typeof value === 'string') {
    jsonBytes = encodedJsonStringBytes(value, MAX_CONTEXT_JSON_BYTES);
    if (jsonBytes > MAX_CONTEXT_JSON_BYTES) return undefined;
  } else return undefined;
  return Object.freeze({
    kind: 'primitive',
    measurement: Object.freeze({ jsonBytes, keyBytes: 0, structureEntries: 0 }),
    value,
  }) as PrimitiveSnapshot;
}

function spendStructure(budget: TraversalBudget): boolean {
  budget.structureEntries += 1;
  return budget.structureEntries <= MAX_CONTEXT_STRUCTURE_ENTRIES;
}

function snapshotNode(source: object, budget: TraversalBudget): ContextSnapshotNode | undefined {
  if (!spendStructure(budget)) return undefined;
  const prototype = Object.getPrototypeOf(source) as unknown;
  const array = Array.isArray(source);
  if ((array && prototype !== Array.prototype) || (!array && prototype !== Object.prototype)) {
    return undefined;
  }
  const entries: ContextSnapshotEntry[] = [];
  if (array) {
    const lengthDescriptor = Object.getOwnPropertyDescriptor(source, 'length');
    if (
      !lengthDescriptor ||
      !('value' in lengthDescriptor) ||
      !Number.isSafeInteger(lengthDescriptor.value) ||
      lengthDescriptor.value < 0 ||
      lengthDescriptor.value > MAX_CONTEXT_STRUCTURE_ENTRIES
    ) {
      return undefined;
    }
    for (let index = 0; index < lengthDescriptor.value; index += 1) {
      if (!spendStructure(budget)) return undefined;
      const key = `${index}`;
      const descriptor = Object.getOwnPropertyDescriptor(source, key);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      entries.push({ encodedKeyBytes: 0, key, snapshot: undefined, sourceValue: descriptor.value });
    }
  } else {
    for (const key in source as Record<string, unknown>) {
      const descriptor = Object.getOwnPropertyDescriptor(source, key);
      if (!descriptor) continue;
      if (!descriptor.enumerable || !('value' in descriptor) || !spendStructure(budget)) {
        return undefined;
      }
      const encodedKeyBytes = encodedJsonStringBytes(key, MAX_CONTEXT_ENCODED_KEY_BYTES);
      budget.keyBytes = boundedSum(budget.keyBytes, encodedKeyBytes, MAX_CONTEXT_ENCODED_KEY_BYTES);
      if (budget.keyBytes > MAX_CONTEXT_ENCODED_KEY_BYTES) return undefined;
      entries.push({ encodedKeyBytes, key, snapshot: undefined, sourceValue: descriptor.value });
    }
  }
  return { array, entries, kind: 'node', source };
}

function snapshotContextGraph(value: unknown): ContextSnapshotGraph | undefined {
  if (typeof value !== 'object' || value === null) return undefined;
  const budget: TraversalBudget = { keyBytes: 0, structureEntries: 0 };
  try {
    const root = snapshotNode(value, budget);
    if (!root || root.array) return undefined;
    const active = new Set<object>([value]);
    const nodes: ContextSnapshotNode[] = [root];
    const stack: TraversalFrame[] = [{ index: 0, node: root }];
    while (stack.length > 0) {
      const frame = stack[stack.length - 1];
      if (!frame) return undefined;
      if (frame.index >= frame.node.entries.length) {
        active.delete(frame.node.source);
        stack.pop();
        continue;
      }
      const entry = frame.node.entries[frame.index];
      frame.index += 1;
      if (!entry) return undefined;
      const primitive = snapshotPrimitive(entry.sourceValue);
      if (primitive) {
        entry.snapshot = primitive;
        continue;
      }
      if (
        typeof entry.sourceValue !== 'object' ||
        entry.sourceValue === null ||
        active.has(entry.sourceValue)
      ) {
        return undefined;
      }
      const child = snapshotNode(entry.sourceValue, budget);
      if (!child) return undefined;
      entry.snapshot = child;
      nodes.push(child);
      active.add(entry.sourceValue);
      stack.push({ index: 0, node: child });
    }
    return Object.freeze({ nodes: Object.freeze(nodes), root });
  } catch {
    return undefined;
  }
}

function cloneSnapshotGraph(graph: ContextSnapshotGraph): CopiedContribution | undefined {
  const completed = new Map<ContextSnapshotNode, ClonedContextValue>();
  try {
    for (let nodeIndex = graph.nodes.length - 1; nodeIndex >= 0; nodeIndex -= 1) {
      const node = graph.nodes[nodeIndex];
      if (!node) return undefined;
      const output: Record<string, unknown> | unknown[] = node.array ? [] : {};
      let jsonBytes = 2;
      let keyBytes = 0;
      let structureEntries = 1;
      for (let entryIndex = 0; entryIndex < node.entries.length; entryIndex += 1) {
        const entry = node.entries[entryIndex];
        if (!entry?.snapshot) return undefined;
        const child =
          entry.snapshot.kind === 'node' ? completed.get(entry.snapshot) : entry.snapshot;
        if (!child) return undefined;
        const prefixBytes =
          (entryIndex === 0 ? 0 : 1) + (node.array ? 0 : entry.encodedKeyBytes + 1);
        jsonBytes = boundedSum(jsonBytes, prefixBytes, MAX_CONTEXT_JSON_BYTES);
        jsonBytes = boundedSum(jsonBytes, child.measurement.jsonBytes, MAX_CONTEXT_JSON_BYTES);
        keyBytes = boundedSum(
          keyBytes,
          entry.encodedKeyBytes + child.measurement.keyBytes,
          MAX_CONTEXT_ENCODED_KEY_BYTES
        );
        structureEntries = boundedSum(
          structureEntries,
          1 + child.measurement.structureEntries,
          MAX_CONTEXT_STRUCTURE_ENTRIES
        );
        if (
          jsonBytes > MAX_CONTEXT_JSON_BYTES ||
          keyBytes > MAX_CONTEXT_ENCODED_KEY_BYTES ||
          structureEntries > MAX_CONTEXT_STRUCTURE_ENTRIES
        ) {
          return undefined;
        }
        Object.defineProperty(output, entry.key, {
          configurable: true,
          enumerable: true,
          value: child.value,
          writable: true,
        });
      }
      completed.set(
        node,
        Object.freeze({
          measurement: Object.freeze({ jsonBytes, keyBytes, structureEntries }),
          value: Object.freeze(output),
        })
      );
    }

    const entries = graph.root.entries.map((entry): CopiedContributionEntry | undefined => {
      if (!entry.snapshot) return undefined;
      const child = entry.snapshot.kind === 'node' ? completed.get(entry.snapshot) : entry.snapshot;
      if (!child) return undefined;
      return Object.freeze({
        jsonBytes: entry.encodedKeyBytes + 1 + child.measurement.jsonBytes,
        key: entry.key,
        keyBytes: entry.encodedKeyBytes + child.measurement.keyBytes,
        structureEntries: 1 + child.measurement.structureEntries,
        value: child.value,
      });
    });
    return entries.some((entry) => entry === undefined)
      ? undefined
      : Object.freeze({ entries: Object.freeze(entries as CopiedContributionEntry[]) });
  } catch {
    return undefined;
  }
}

function copyContribution(value: unknown): CopiedContribution | undefined {
  const graph = snapshotContextGraph(value);
  return graph ? cloneSnapshotGraph(graph) : undefined;
}

class AuctionContextRegistryOwner implements AuctionContextRegistry {
  private readonly manifestIntegrationIds: readonly string[];
  private readonly manifestSet: ReadonlySet<string>;
  private readonly runtimeGeneration: object | undefined;
  private readonly registrations = new Map<string, ContributorRecord>();
  private readonly runtimeOwner: ContextContributorOwner;
  private readonly onContributorFailure:
    ((failure: AuctionContextContributorFailure) => void) | undefined;
  private isDisposed = false;

  public constructor(options: AuctionContextRegistryOptions) {
    this.manifestIntegrationIds = snapshotManifest(options.manifestIntegrationIds);
    this.manifestSet = new Set(this.manifestIntegrationIds);
    this.runtimeOwner = options.runtimeOwner;
    this.runtimeGeneration = this.snapshotCurrentOwnerGeneration(options.runtimeOwner);
    this.onContributorFailure = options.onContributorFailure;
    if (this.runtimeGeneration === undefined) {
      this.dispose();
      return;
    }
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
    const ownerGeneration = this.snapshotCurrentOwnerGeneration(owner);
    if (
      !this.runtimeIsCurrent() ||
      !this.manifestSet.has(integrationId) ||
      this.registrations.has(integrationId) ||
      typeof contributor !== 'function' ||
      ownerGeneration === undefined
    ) {
      return false;
    }
    const record: ContributorRecord = Object.freeze({
      contributor,
      owner,
      ownerGeneration,
    });
    this.registrations.set(integrationId, record);
    try {
      owner.onDispose('auction-context-contributor', () => {
        this.deleteRegistration(integrationId, record);
      });
    } catch {
      this.deleteRegistration(integrationId, record);
      return false;
    }
    const runtimeIsCurrent = this.runtimeIsCurrent();
    const recordSurvivedRuntimeReflection = this.registrations.get(integrationId) === record;
    if (!runtimeIsCurrent || !recordSurvivedRuntimeReflection) {
      this.deleteRegistration(integrationId, record);
      return false;
    }
    const ownerIsCurrent = this.ownerIsCurrent(owner, ownerGeneration);
    const recordSurvivedOwnerReflection = this.registrations.get(integrationId) === record;
    if (!ownerIsCurrent || !recordSurvivedOwnerReflection) {
      this.deleteRegistration(integrationId, record);
      return false;
    }
    return true;
  }

  public snapshot(): Readonly<Record<string, unknown>> {
    if (!this.runtimeIsCurrent()) {
      this.dispose();
      return Object.freeze({});
    }
    const accepted = new Map<string, AcceptedContributionEntry>();
    let acceptedEntryBytes = 0;
    let acceptedKeyBytes = 0;
    let acceptedStructureEntries = 1;
    for (const integrationId of this.manifestIntegrationIds) {
      const record = this.registrations.get(integrationId);
      if (!record) continue;
      const ownerIsCurrentBeforeInvocation = this.ownerIsCurrent(
        record.owner,
        record.ownerGeneration
      );
      const recordSurvivedOwnerReflection = this.registrations.get(integrationId) === record;
      if (!ownerIsCurrentBeforeInvocation || !recordSurvivedOwnerReflection) continue;
      let contribution: CopiedContribution | undefined;
      let contributorReturnedUndefined = false;
      try {
        const candidate = record.contributor();
        contributorReturnedUndefined = candidate === undefined;
        if (!contributorReturnedUndefined) contribution = copyContribution(candidate);
      } catch {
        contribution = undefined;
      }
      if (this.registrations.get(integrationId) !== record) continue;
      if (contributorReturnedUndefined) continue;
      if (!contribution) {
        this.reportFailure(integrationId);
        continue;
      }
      const runtimeIsCurrent = this.runtimeIsCurrent();
      const recordSurvivedRuntimeReflection = this.registrations.get(integrationId) === record;
      if (!runtimeIsCurrent) return Object.freeze({});
      if (!recordSurvivedRuntimeReflection) continue;
      const ownerIsCurrentBeforeMerge = this.ownerIsCurrent(record.owner, record.ownerGeneration);
      const recordSurvivedOwnerReflectionBeforeMerge =
        this.registrations.get(integrationId) === record;
      if (!ownerIsCurrentBeforeMerge || !recordSurvivedOwnerReflectionBeforeMerge) continue;
      let prospectiveEntryBytes = acceptedEntryBytes;
      let prospectiveKeyBytes = acceptedKeyBytes;
      let prospectiveStructureEntries = acceptedStructureEntries;
      let prospectiveEntryCount = accepted.size;
      for (const entry of contribution.entries) {
        const predecessor = accepted.get(entry.key);
        if (predecessor) {
          prospectiveEntryBytes -= predecessor.contribution.jsonBytes;
          prospectiveKeyBytes -= predecessor.contribution.keyBytes;
          prospectiveStructureEntries -= predecessor.contribution.structureEntries;
        } else prospectiveEntryCount += 1;
        prospectiveEntryBytes += entry.jsonBytes;
        prospectiveKeyBytes += entry.keyBytes;
        prospectiveStructureEntries += entry.structureEntries;
      }
      const prospectiveJsonBytes =
        2 + Math.max(0, prospectiveEntryCount - 1) + prospectiveEntryBytes;
      if (
        prospectiveJsonBytes > MAX_CONTEXT_JSON_BYTES ||
        prospectiveKeyBytes > MAX_CONTEXT_ENCODED_KEY_BYTES ||
        prospectiveStructureEntries > MAX_CONTEXT_STRUCTURE_ENTRIES
      ) {
        this.reportFailure(integrationId);
        continue;
      }
      if (this.registrations.get(integrationId) !== record) continue;
      for (const entry of contribution.entries) {
        accepted.set(entry.key, Object.freeze({ contribution: entry, integrationId, record }));
      }
      acceptedEntryBytes = prospectiveEntryBytes;
      acceptedKeyBytes = prospectiveKeyBytes;
      acceptedStructureEntries = prospectiveStructureEntries;
    }
    try {
      const output: Record<string, unknown> = {};
      for (const [key, entry] of accepted) {
        Object.defineProperty(output, key, {
          configurable: true,
          enumerable: true,
          value: entry.contribution.value,
          writable: true,
        });
      }
      if (!this.runtimeIsCurrent()) return Object.freeze({});
      for (const entry of accepted.values()) {
        if (this.registrations.get(entry.integrationId) !== entry.record) {
          return Object.freeze({});
        }
      }
      return Object.freeze(output);
    } catch {
      return Object.freeze({});
    }
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
    const generation = this.runtimeGeneration;
    if (this.isDisposed || generation === undefined) return false;
    return this.ownerIsCurrent(this.runtimeOwner, generation) && !this.isDisposed;
  }

  private ownerIsCurrent(owner: ContextContributorOwner, generation: object): boolean {
    try {
      return owner.generation === generation && owner.isCurrent();
    } catch {
      return false;
    }
  }

  private snapshotCurrentOwnerGeneration(owner: ContextContributorOwner): object | undefined {
    try {
      const generation = owner.generation;
      if (typeof generation !== 'object' || generation === null) return undefined;
      return owner.isCurrent() ? generation : undefined;
    } catch {
      return undefined;
    }
  }

  private deleteRegistration(integrationId: string, record: ContributorRecord): void {
    if (this.registrations.get(integrationId) === record) {
      this.registrations.delete(integrationId);
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
