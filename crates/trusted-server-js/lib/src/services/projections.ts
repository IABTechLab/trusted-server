import type { NavigationSession } from '../kernel/sessions';

/** Shared maximum across server-projected and programmatically admitted slots. */
export const MAX_ACTIVE_SLOT_RECORDS = 256;

/** Exact projection parser injected by the composition root. */
export type AuctionProjectionParser = (candidate: unknown) => object | undefined;

/** A reversible reservation prepared without mutating the live slot registry. */
export interface PreparedProjectionSlots {
  readonly ownerGeneration: object;
  readonly commit: () => boolean;
  readonly rollback: () => void;
}

/** Exact slot identity and DOM aliases reserved with one admitted projection. */
export interface ProjectionSlotRegistration {
  readonly registeredSlotId: string;
  readonly domAliases: readonly string[];
}

/** Slot-registry transaction boundary consumed by the page-bids controller. */
export interface ProjectionSlotRegistry {
  readonly prepareProjectionSlots: (
    ownerGeneration: object,
    slots: readonly ProjectionSlotRegistration[],
    maximumActiveSlots: number
  ) => PreparedProjectionSlots | undefined;
}

/** Result of one current-generation page-bids response. */
export type PageBidsCommitResult =
  | Readonly<{ status: 'committed' }>
  | Readonly<{
      status: 'rejected';
      reason: 'capacity' | 'duplicate' | 'malformed' | 'stale';
    }>;

/** One-response controller bound to a single navigation generation. */
export interface PageBidsController {
  readonly commit: (candidate: unknown) => PageBidsCommitResult;
}

/** Dependencies for one navigation-bound page-bids controller. */
export interface PageBidsControllerOptions {
  readonly navigation: NavigationSession;
  readonly parseProjection: AuctionProjectionParser;
  readonly slotRegistry: ProjectionSlotRegistry;
}

const COMMITTED = Object.freeze({ status: 'committed' as const });
const rejected = {
  capacity: Object.freeze({ status: 'rejected' as const, reason: 'capacity' as const }),
  duplicate: Object.freeze({ status: 'rejected' as const, reason: 'duplicate' as const }),
  malformed: Object.freeze({ status: 'rejected' as const, reason: 'malformed' as const }),
  stale: Object.freeze({ status: 'rejected' as const, reason: 'stale' as const }),
};

function recursivelyFreeze(value: unknown, visited = new Set<object>()): boolean {
  if ((typeof value !== 'object' && typeof value !== 'function') || value === null) return true;
  if (visited.has(value)) return true;
  visited.add(value);
  try {
    for (const key of Reflect.ownKeys(value)) {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (
        !descriptor ||
        !('value' in descriptor) ||
        !recursivelyFreeze(descriptor.value, visited)
      ) {
        return false;
      }
    }
    Object.freeze(value);
    return true;
  } catch {
    return false;
  }
}

function projectedSlots(projection: object): readonly ProjectionSlotRegistration[] | undefined {
  try {
    const slotsDescriptor = Object.getOwnPropertyDescriptor(projection, 'slots');
    if (!slotsDescriptor || !('value' in slotsDescriptor)) return undefined;
    const projected = slotsDescriptor.value;
    if (!Array.isArray(projected) || projected.length > MAX_ACTIVE_SLOT_RECORDS) return undefined;
    const slots: ProjectionSlotRegistration[] = [];
    const seen = new Set<string>();
    for (const placement of projected) {
      if (typeof placement !== 'object' || placement === null) return undefined;
      const slotDescriptor = Object.getOwnPropertyDescriptor(placement, 'slot');
      const divDescriptor = Object.getOwnPropertyDescriptor(placement, 'divId');
      if (
        !slotDescriptor ||
        !('value' in slotDescriptor) ||
        typeof slotDescriptor.value !== 'string' ||
        !divDescriptor ||
        !('value' in divDescriptor) ||
        typeof divDescriptor.value !== 'string'
      ) {
        return undefined;
      }
      if (seen.has(slotDescriptor.value)) return undefined;
      seen.add(slotDescriptor.value);
      slots.push(
        Object.freeze({
          registeredSlotId: slotDescriptor.value,
          domAliases: Object.freeze([divDescriptor.value]),
        })
      );
    }
    return Object.freeze(slots);
  } catch {
    return undefined;
  }
}

function rollback(reservation: PreparedProjectionSlots): void {
  try {
    reservation.rollback();
  } catch {
    // Rollback is best-effort; a conforming slot transaction is reversibly prepared.
  }
}

/** Parse, deep-copy through the injected parser, and recursively freeze initial boot input. */
export function prepareInitialAuctionProjection(
  candidate: unknown,
  parseProjection: AuctionProjectionParser
): Readonly<object> | undefined {
  try {
    const parsed = parseProjection(candidate);
    if (!parsed || !recursivelyFreeze(parsed)) return undefined;
    return parsed;
  } catch {
    return undefined;
  }
}

/** Construct one transactional page-bids controller for a navigation generation. */
export function createPageBidsController(options: PageBidsControllerOptions): PageBidsController {
  const ownerGeneration = options.navigation.generation;
  let didCommit = options.navigation.currentAuctionProjection !== undefined;

  return Object.freeze({
    commit(candidate: unknown): PageBidsCommitResult {
      if (!options.navigation.isCurrent() || options.navigation.generation !== ownerGeneration) {
        return rejected.stale;
      }
      if (didCommit || options.navigation.currentAuctionProjection !== undefined) {
        return rejected.duplicate;
      }

      const projection = prepareInitialAuctionProjection(candidate, options.parseProjection);
      if (!projection) return rejected.malformed;
      const slots = projectedSlots(projection);
      if (!slots) return rejected.malformed;
      if (!options.navigation.isCurrent()) return rejected.stale;

      let reservation: PreparedProjectionSlots | undefined;
      try {
        reservation = options.slotRegistry.prepareProjectionSlots(
          ownerGeneration,
          slots,
          MAX_ACTIVE_SLOT_RECORDS
        );
      } catch {
        return rejected.capacity;
      }
      if (!reservation || reservation.ownerGeneration !== ownerGeneration) {
        if (reservation) rollback(reservation);
        return rejected.capacity;
      }
      if (!options.navigation.isCurrent()) {
        rollback(reservation);
        return rejected.stale;
      }

      try {
        if (!reservation.commit()) {
          rollback(reservation);
          return rejected.capacity;
        }
      } catch {
        rollback(reservation);
        return rejected.capacity;
      }
      if (!options.navigation.isCurrent()) {
        rollback(reservation);
        return rejected.stale;
      }
      if (!options.navigation.installAuctionProjection(projection)) {
        rollback(reservation);
        return options.navigation.isCurrent() ? rejected.duplicate : rejected.stale;
      }
      didCommit = true;
      return COMMITTED;
    },
  });
}
