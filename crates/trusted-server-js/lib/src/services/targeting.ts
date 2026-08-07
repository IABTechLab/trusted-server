/** The narrow GPT slot targeting surface consumed by the journal. */
export interface TargetingBoundary {
  readonly clearTargeting: (key?: string) => unknown;
  readonly getTargeting: (key: string) => readonly string[];
  readonly setTargeting: (key: string, value: string | readonly string[]) => unknown;
}

/** One opaque targeting ownership frame. */
export interface TargetingOwnership {
  readonly ownerId: string;
  readonly release: () => void;
}

/** Frozen ownership inventory exposed only to tests. */
export interface TargetingInventorySnapshot {
  readonly frames: number;
  readonly slots: number;
}

/** Owner-aware targeting operations exposed to render services. */
export interface TargetingService {
  readonly dispose: () => void;
  readonly disposeOwner: (ownerId: string) => void;
  readonly invalidatePublisherMutation: (slot: object, key?: string) => void;
  readonly observePublisherMutations: (
    slot: object,
    adapter: GoogletagAdapter
  ) => GoogletagOperation<void>;
  readonly own: (
    slot: object,
    key: string,
    value: string,
    ownerId: string,
    targeting: TargetingBoundary
  ) => TargetingOwnership | undefined;
  readonly snapshotForTest: () => TargetingInventorySnapshot;
}

interface PublisherPredecessor {
  readonly kind: 'publisher';
  readonly values: readonly string[];
}

interface TargetingFrame {
  alive: boolean;
  readonly kind: 'frame';
  predecessor: PublisherPredecessor | TargetingFrame;
  readonly boundary: TargetingBoundary;
  readonly installed: string;
  readonly key: string;
  readonly ownerId: string;
  readonly slot: object;
}

interface TargetingChain {
  readonly frames: TargetingFrame[];
}

const mapDeleteIntrinsic = Map.prototype.delete;
const mapGetIntrinsic = Map.prototype.get;
const mapSetIntrinsic = Map.prototype.set;
const mapSizeGetter = Object.getOwnPropertyDescriptor(Map.prototype, 'size')?.get as (
  this: Map<unknown, unknown>
) => number;
const mapIteratorNextIntrinsic = Object.getPrototypeOf(new Map().entries()).next as (
  this: IterableIterator<unknown>
) => IteratorResult<unknown>;
const weakMapDeleteIntrinsic = WeakMap.prototype.delete;
const weakMapGetIntrinsic = WeakMap.prototype.get;
const weakMapSetIntrinsic = WeakMap.prototype.set;

function mapValue<Key, Value>(map: Map<Key, Value>, key: Key): Value | undefined {
  return Reflect.apply(mapGetIntrinsic, map, [key]) as Value | undefined;
}

function setMapValue<Key, Value>(map: Map<Key, Value>, key: Key, value: Value): void {
  Reflect.apply(mapSetIntrinsic, map, [key, value]);
}

function deleteMapValue<Key, Value>(map: Map<Key, Value>, key: Key): boolean {
  return Reflect.apply(mapDeleteIntrinsic, map, [key]) as boolean;
}

function mapSize(map: Map<unknown, unknown>): number {
  return Reflect.apply(mapSizeGetter, map, []) as number;
}

function weakMapValue<Key extends object, Value>(
  map: WeakMap<Key, Value>,
  key: Key
): Value | undefined {
  return Reflect.apply(weakMapGetIntrinsic, map, [key]) as Value | undefined;
}

function setWeakMapValue<Key extends object, Value>(
  map: WeakMap<Key, Value>,
  key: Key,
  value: Value
): void {
  Reflect.apply(weakMapSetIntrinsic, map, [key, value]);
}

function deleteWeakMapValue<Key extends object, Value>(
  map: WeakMap<Key, Value>,
  key: Key
): boolean {
  return Reflect.apply(weakMapDeleteIntrinsic, map, [key]) as boolean;
}

function exactInstalledValue(values: readonly string[], installed: string): boolean {
  return values.length === 1 && values[0] === installed;
}

function exactValues(left: readonly string[], right: readonly string[]): boolean {
  if (left.length !== right.length) return false;
  for (let index = 0; index < left.length; index += 1) {
    if (left[index] !== right[index]) return false;
  }
  return true;
}

function copyValues(values: readonly string[]): readonly string[] {
  if (!Array.isArray(values)) {
    throw new TypeError('GPT targeting values must be strings');
  }
  const copied: string[] = [];
  for (let index = 0; index < values.length; index += 1) {
    const value = values[index];
    if (typeof value !== 'string') {
      throw new TypeError('GPT targeting values must be strings');
    }
    copied[index] = value;
  }
  return Object.freeze(copied);
}

/** Construct the runtime-owned GPT targeting restoration journal. */
export function createTargetingService(): TargetingService {
  const chainsBySlot = new WeakMap<object, Map<string, TargetingChain>>();
  const liveFrames = new Set<TargetingFrame>();
  const observationReleases = new Set<() => void>();
  const setAddIntrinsic = Set.prototype.add;
  const setDeleteIntrinsic = Set.prototype.delete;
  const setValuesIntrinsic = Set.prototype.values;
  const setIteratorNextIntrinsic = Object.getPrototypeOf(new Set().values()).next as (
    this: IterableIterator<unknown>
  ) => IteratorResult<unknown>;
  let disposed = false;
  let frameCount = 0;
  let slotCount = 0;

  const addLiveFrame = (frame: TargetingFrame): void => {
    Reflect.apply(setAddIntrinsic, liveFrames, [frame]);
  };
  const deleteLiveFrame = (frame: TargetingFrame): boolean =>
    Reflect.apply(setDeleteIntrinsic, liveFrames, [frame]) as boolean;
  const liveFrameValues = (): IterableIterator<TargetingFrame> =>
    Reflect.apply(setValuesIntrinsic, liveFrames, []) as IterableIterator<TargetingFrame>;
  const addObservationRelease = (release: () => void): void => {
    Reflect.apply(setAddIntrinsic, observationReleases, [release]);
  };
  const deleteObservationRelease = (release: () => void): boolean =>
    Reflect.apply(setDeleteIntrinsic, observationReleases, [release]) as boolean;
  const observationValues = (): IterableIterator<() => void> =>
    Reflect.apply(setValuesIntrinsic, observationReleases, []) as IterableIterator<() => void>;
  const setSnapshot = <Value>(iterator: IterableIterator<Value>): Value[] => {
    const values: Value[] = [];
    while (true) {
      const step = Reflect.apply(setIteratorNextIntrinsic, iterator, []) as IteratorResult<Value>;
      if (step.done) return values;
      values[values.length] = step.value;
    }
  };

  const removeEmptySlot = (slot: object, slotChains: Map<string, TargetingChain>): void => {
    if (mapSize(slotChains) !== 0) return;
    if (weakMapValue(chainsBySlot, slot) !== slotChains) return;
    if (deleteWeakMapValue(chainsBySlot, slot)) slotCount -= 1;
  };

  const invalidateChain = (
    slot: object,
    slotChains: Map<string, TargetingChain>,
    key: string,
    chain: TargetingChain
  ): void => {
    if (mapValue(slotChains, key) !== chain) return;
    deleteMapValue(slotChains, key);
    for (let index = 0; index < chain.frames.length; index += 1) {
      const frame = chain.frames[index];
      if (!frame?.alive) continue;
      frame.alive = false;
      frameCount -= 1;
      deleteLiveFrame(frame);
    }
    chain.frames.length = 0;
    removeEmptySlot(slot, slotChains);
  };

  const removeFrame = (
    frame: TargetingFrame,
    slotChains: Map<string, TargetingChain>,
    chain: TargetingChain,
    frameIndex: number
  ): void => {
    const successor = chain.frames[frameIndex + 1];
    if (successor) successor.predecessor = frame.predecessor;
    for (let index = frameIndex; index < chain.frames.length - 1; index += 1) {
      const next = chain.frames[index + 1];
      if (next) chain.frames[index] = next;
    }
    chain.frames.length -= 1;
    frame.alive = false;
    frameCount -= 1;
    deleteLiveFrame(frame);
    if (chain.frames.length === 0) deleteMapValue(slotChains, frame.key);
    removeEmptySlot(frame.slot, slotChains);
  };

  const expectedPredecessor = (frame: TargetingFrame): readonly string[] | undefined => {
    const predecessor = frame.predecessor;
    if (predecessor.kind === 'publisher') return predecessor.values;
    return predecessor.alive ? Object.freeze([predecessor.installed]) : undefined;
  };

  const restorePredecessor = (frame: TargetingFrame): void => {
    const predecessor = frame.predecessor;
    if (predecessor.kind === 'publisher') {
      if (predecessor.values.length === 0) frame.boundary.clearTargeting(frame.key);
      else frame.boundary.setTargeting(frame.key, predecessor.values);
    } else if (predecessor.alive) {
      frame.boundary.setTargeting(frame.key, predecessor.installed);
    }
  };

  const release = (frame: TargetingFrame): boolean => {
    if (!frame.alive) return true;
    const slotChains = weakMapValue(chainsBySlot, frame.slot);
    const chain = slotChains ? mapValue(slotChains, frame.key) : undefined;
    if (!slotChains || !chain) {
      frame.alive = false;
      frameCount -= 1;
      deleteLiveFrame(frame);
      return true;
    }
    let frameIndex = -1;
    for (let index = 0; index < chain.frames.length; index += 1) {
      if (chain.frames[index] === frame) {
        frameIndex = index;
        break;
      }
    }
    if (frameIndex < 0) {
      frame.alive = false;
      frameCount -= 1;
      deleteLiveFrame(frame);
      return true;
    }

    const wasTop = frameIndex === chain.frames.length - 1;
    if (!wasTop) {
      removeFrame(frame, slotChains, chain, frameIndex);
      return true;
    }

    let actual: readonly string[];
    try {
      actual = copyValues(frame.boundary.getTargeting(frame.key));
    } catch {
      return false;
    }
    if (!exactInstalledValue(actual, frame.installed)) {
      invalidateChain(frame.slot, slotChains, frame.key, chain);
      return true;
    }

    const expected = expectedPredecessor(frame);
    if (!expected) return false;
    try {
      restorePredecessor(frame);
    } catch {
      // The post-failure read below distinguishes mutate-then-throw from no mutation.
    }
    let restored: readonly string[];
    try {
      restored = copyValues(frame.boundary.getTargeting(frame.key));
    } catch {
      return false;
    }
    let exact = restored.length === expected.length;
    for (let index = 0; exact && index < restored.length; index += 1) {
      exact = restored[index] === expected[index];
    }
    if (!exact) {
      return false;
    }
    removeFrame(frame, slotChains, chain, frameIndex);
    return true;
  };

  const rollbackFailedInstallation = (frame: TargetingFrame): boolean => {
    const slotChains = weakMapValue(chainsBySlot, frame.slot);
    const chain = slotChains ? mapValue(slotChains, frame.key) : undefined;
    if (!slotChains || !chain) return true;
    const frameIndex = chain.frames.length - 1;
    if (chain.frames[frameIndex] !== frame) return false;
    let actual: readonly string[];
    try {
      actual = copyValues(frame.boundary.getTargeting(frame.key));
    } catch {
      return false;
    }
    const predecessor = expectedPredecessor(frame);
    if (predecessor && exactValues(actual, predecessor)) {
      removeFrame(frame, slotChains, chain, frameIndex);
      return true;
    }
    if (exactInstalledValue(actual, frame.installed)) return release(frame);
    return false;
  };

  const invalidatePublisherMutation = (slot: object, key?: string): void => {
    const slotChains = weakMapValue(chainsBySlot, slot);
    if (!slotChains) return;
    if (key !== undefined) {
      const chain = mapValue(slotChains, key);
      if (chain) invalidateChain(slot, slotChains, key, chain);
      return;
    }
    const entries: Array<readonly [string, TargetingChain]> = [];
    const mapEntriesIntrinsic = Map.prototype.entries;
    const iterator = Reflect.apply(mapEntriesIntrinsic, slotChains, []) as IterableIterator<
      [string, TargetingChain]
    >;
    while (true) {
      const step = Reflect.apply(mapIteratorNextIntrinsic, iterator, []) as IteratorResult<
        [string, TargetingChain]
      >;
      if (step.done) break;
      entries[entries.length] = step.value;
    }
    for (const [entryKey, chain] of entries) invalidateChain(slot, slotChains, entryKey, chain);
  };

  const own = (
    slot: object,
    key: string,
    value: string,
    ownerId: string,
    targeting: TargetingBoundary
  ): TargetingOwnership | undefined => {
    if (disposed) return undefined;
    if ((typeof slot !== 'object' || slot === null) && typeof slot !== 'function') {
      throw new TypeError('GPT slot object required');
    }
    if (key.length === 0 || value.length === 0 || ownerId.length === 0) {
      throw new TypeError('Targeting key, value, and owner are required');
    }

    const actual = copyValues(targeting.getTargeting(key));
    let slotChains = weakMapValue(chainsBySlot, slot);
    let chain = slotChains ? mapValue(slotChains, key) : undefined;
    const top = chain?.frames[chain.frames.length - 1];
    if (top && !exactInstalledValue(actual, top.installed)) {
      invalidateChain(
        slot,
        slotChains as Map<string, TargetingChain>,
        key,
        chain as TargetingChain
      );
      slotChains = weakMapValue(chainsBySlot, slot);
      chain = undefined;
    }
    if (!slotChains) slotChains = new Map<string, TargetingChain>();
    if (!chain) chain = { frames: [] };
    const currentTop = chain.frames[chain.frames.length - 1];
    const frame: TargetingFrame = {
      alive: true,
      kind: 'frame',
      predecessor: currentTop ?? { kind: 'publisher', values: actual },
      boundary: targeting,
      installed: value,
      key,
      ownerId,
      slot,
    };
    const wasNewSlot = weakMapValue(chainsBySlot, slot) === undefined;
    const wasNewChain = mapValue(slotChains, key) === undefined;
    let publishedWeakMap = false;
    let publishedChain = false;
    let publishedFrame = false;
    try {
      if (wasNewSlot) {
        setWeakMapValue(chainsBySlot, slot, slotChains);
        if (weakMapValue(chainsBySlot, slot) !== slotChains) throw new Error('journal publication');
        publishedWeakMap = true;
        slotCount += 1;
      }
      if (wasNewChain) {
        setMapValue(slotChains, key, chain);
        if (mapValue(slotChains, key) !== chain) throw new Error('journal publication');
        publishedChain = true;
      }
      chain.frames[chain.frames.length] = frame;
      publishedFrame = true;
      addLiveFrame(frame);
      frameCount += 1;
    } catch (error) {
      if (publishedFrame) chain.frames.length -= 1;
      frame.alive = false;
      if (deleteLiveFrame(frame) && frameCount > 0) frameCount -= 1;
      if ((publishedChain || mapValue(slotChains, key) === chain) && chain.frames.length === 0) {
        deleteMapValue(slotChains, key);
      }
      if (weakMapValue(chainsBySlot, slot) === slotChains && mapSize(slotChains) === 0) {
        deleteWeakMapValue(chainsBySlot, slot);
        if (publishedWeakMap && slotCount > 0) slotCount -= 1;
      }
      throw error;
    }

    try {
      targeting.setTargeting(key, value);
    } catch (error) {
      rollbackFailedInstallation(frame);
      throw error;
    }

    let released = false;
    return Object.freeze({
      ownerId,
      release: (): void => {
        if (released) return;
        released = release(frame);
      },
    });
  };

  return Object.freeze({
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      const frames = setSnapshot(liveFrameValues());
      for (let index = frames.length - 1; index >= 0; index -= 1) {
        const frame = frames[index];
        if (frame) release(frame);
      }
      const observations = setSnapshot(observationValues());
      for (let index = observations.length - 1; index >= 0; index -= 1) {
        try {
          observations[index]?.();
        } catch {
          // Adapter wrapper cleanup cannot escape service disposal.
        }
      }
    },
    disposeOwner: (ownerId: string): void => {
      const frames = setSnapshot(liveFrameValues());
      for (let index = frames.length - 1; index >= 0; index -= 1) {
        const frame = frames[index];
        if (frame?.ownerId === ownerId) release(frame);
      }
    },
    invalidatePublisherMutation,
    observePublisherMutations: (slot: object, adapter: GoogletagAdapter) => {
      let ownedRelease = (): void => undefined;
      const operation = adapter.run<void>((gpt) => {
        if (disposed) return;
        let release = gpt.observeTargeting(
          slot,
          Object.freeze({
            beforePublisherMutation: (mutatedSlot: object, key?: string) => {
              invalidatePublisherMutation(mutatedSlot, key);
            },
          })
        );
        let active = true;
        ownedRelease = (): void => {
          if (!active) return;
          active = false;
          deleteObservationRelease(ownedRelease);
          const current = release;
          release = (): void => undefined;
          current();
        };
        try {
          addObservationRelease(ownedRelease);
        } catch (error) {
          ownedRelease();
          throw error;
        }
        if (disposed) ownedRelease();
      });
      void operation.result.catch(() => {
        ownedRelease();
      });
      return Object.freeze({
        get status() {
          return operation.status;
        },
        result: operation.result,
        dispose: (): void => {
          ownedRelease();
          operation.dispose();
        },
      });
    },
    own,
    snapshotForTest: () => Object.freeze({ frames: frameCount, slots: slotCount }),
  });
}
import type { GoogletagAdapter, GoogletagOperation } from '../adapters/googletag';
