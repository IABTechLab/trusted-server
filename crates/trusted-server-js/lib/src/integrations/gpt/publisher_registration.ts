import { claimFirstImpressionForTrustedServer } from '../../core/first_impression';
import type {
  AuctionBidData,
  AuctionSlot,
  FirstImpressionSlotClaim,
  PublisherGptSlot,
  PublisherSlotRegistration,
  PublisherSlotRegistrationOptions,
  PublisherSlotRegistrationRecord,
  PublisherSlotRegistrationState,
  PublisherSlotRegistrationStatus,
  Size,
  TsjsApi,
} from '../../core/types';

interface RegistrationTag {
  cmd?: { push(callback: () => void): unknown };
  getConfig?(key: string): { disableInitialLoad?: boolean };
  pubads?(): {
    getSlots?(): PublisherGptSlot[];
    refresh(slots: PublisherGptSlot[]): void;
    addEventListener?(name: string, listener: (event: { slot: PublisherGptSlot }) => void): void;
  };
  display?(id: string): void;
}

const CANDIDATE_KEYS = ['hb_pb', 'hb_bidder', 'hb_adid', 'hb_cache_host', 'hb_cache_path'] as const;

function registrationTag(): RegistrationTag | undefined {
  return (window as unknown as { googletag?: RegistrationTag }).googletag;
}

/** Retire old records before accepting another navigation's registrations. */
export function invalidatePublisherSlotRegistrations(ts: TsjsApi): void {
  const previous = ts.publisherSlotRegistrationState;
  for (const record of Object.values(previous?.records ?? {})) {
    record.status = { status: 'cancelled', reason: 'navigation' };
    record.element = undefined;
    record.gptSlot = undefined;
  }
  ts.publisherSlotRegistrationState = {
    generation: ts.navGeneration ?? 0,
    nextToken: previous?.nextToken ?? 0,
    records: Object.create(null) as PublisherSlotRegistrationState['records'],
  };
}

function registrationState(ts: TsjsApi): PublisherSlotRegistrationState {
  if (ts.publisherSlotRegistrationState?.generation !== (ts.navGeneration ?? 0)) {
    invalidatePublisherSlotRegistrations(ts);
  }
  return ts.publisherSlotRegistrationState!;
}

/** Only accepted initial-seam or successful current SPA responses may publish admission. */
export function settlePublisherSlotPayload(
  ts: TsjsApi,
  generation: number,
  slots: AuctionSlot[],
  bids: Record<string, AuctionBidData>
): void {
  if ((ts.navGeneration ?? 0) !== generation) return;
  registrationState(ts).payload = { generation, slots, bids };
  ts.publisherSlotRegistrationDispatch?.();
}

/** Automatic passes own only their entries; registered entries derive from consumed records. */
export function retainPublisherSlotMappings(
  ts: TsjsApi,
  mapping: Record<string, string>,
  targetingKeys: Record<string, string[]>
): void {
  const state = registrationState(ts);
  for (const record of Object.values(state.records)) {
    if (!record.consumed || !record.targetingKeys) continue;
    mapping[record.elementId] = record.slotId;
    targetingKeys[record.elementId] = record.targetingKeys;
  }
}

/** An excluded registered bid cannot be recovered later from the global server bid map. */
export function registeredCandidateAllowed(
  ts: TsjsApi,
  slotId: string,
  bid: AuctionBidData
): boolean {
  const record =
    ts.publisherSlotRegistrationState?.generation === (ts.navGeneration ?? 0)
      ? ts.publisherSlotRegistrationState.records[slotId]
      : undefined;
  if (record) {
    if (
      !record.consumed ||
      record.selectedCandidate !== bid ||
      !record.element?.isConnected ||
      record.element.id !== record.elementId ||
      document.getElementById(record.elementId) !== record.element
    )
      return false;
    try {
      return (
        !!record.gptSlot && !!registrationTag()?.pubads?.().getSlots?.().includes(record.gptSlot)
      );
    } catch {
      return false;
    }
  }
  return !ts.adSlots?.some(
    (slot) => slot.id === slotId && slot.initialization === 'publisher_registered'
  );
}

/** Observe only an exact current native registration's render, not an ID alias. */
export function observePublisherSlotRender(ts: TsjsApi, native: PublisherGptSlot): void {
  const state = ts.publisherSlotRegistrationState;
  if (state?.generation !== (ts.navGeneration ?? 0)) return;
  for (const record of Object.values(state.records)) {
    if (
      record.gptSlot === native &&
      record.consumed &&
      record.element?.isConnected &&
      record.element.id === record.elementId &&
      document.getElementById(record.elementId) === record.element &&
      (record.status.status === 'requested' || record.status.status === 'dispatching')
    ) {
      record.status = { status: 'rendered' };
    }
  }
}

function finiteSize(size: unknown): size is Size {
  return (
    Array.isArray(size) &&
    size.length === 2 &&
    size.every(
      (value: unknown) => typeof value === 'number' && Number.isInteger(value) && value > 0
    )
  );
}

function normalizedFormats(formats: ReadonlyArray<Size>): Size[] {
  return [
    ...new Map(formats.map((size) => [size.join('x'), [size[0], size[1]] as Size])).values(),
  ].sort((left, right) => left[0] - right[0] || left[1] - right[1]);
}

function loadPolicy(
  ts: TsjsApi,
  tag: RegistrationTag | undefined
): PublisherSlotRegistrationStatus | undefined {
  try {
    const effective = tag?.getConfig?.('disableInitialLoad')?.disableInitialLoad;
    if (effective !== undefined) ts.gptInitialLoadDisabled = effective === true;
    if (effective === true || (effective === undefined && ts.gptInitialLoadDisabled === true))
      return;
    return {
      status: 'rejected',
      reason:
        effective === false || ts.gptInitialLoadDisabled === false
          ? 'initial_load_enabled'
          : 'initial_load_unknown',
    };
  } catch {
    return { status: 'rejected', reason: 'initial_load_unknown' };
  }
}

function exactElement(id: string): {
  element?: HTMLElement;
  error?: PublisherSlotRegistrationStatus;
} {
  const element = document.getElementById(id);
  if (!id || !element?.isConnected)
    return { error: { status: 'rejected', reason: 'missing_element' } };
  if (
    Array.from(document.querySelectorAll('[id]')).filter((candidate) => candidate.id === id)
      .length !== 1
  ) {
    return { error: { status: 'rejected', reason: 'duplicate_dom_id' } };
  }
  return { element };
}

function registrationHandle(
  ts: TsjsApi,
  record: PublisherSlotRegistrationRecord
): PublisherSlotRegistration {
  return {
    getStatus: () => {
      if (record.generation !== (ts.navGeneration ?? 0) && !record.consumed) {
        record.status = { status: 'cancelled', reason: 'navigation' };
      }
      return { ...record.status };
    },
    cancel: () => {
      if (record.status.status === 'rejected' || record.status.status === 'cancelled') return;
      record.status = { status: 'cancelled', reason: 'explicit' };
      if (!record.consumed) {
        record.element = undefined;
        record.gptSlot = undefined;
      }
    },
  };
}

/** Install protocol v1 without replacing browser-local records or committed requests. */
export function installPublisherSlotRegistration(
  ts: TsjsApi,
  claim: (element: HTMLElement) => FirstImpressionSlotClaim | undefined = (element) =>
    claimFirstImpressionForTrustedServer(ts, element)
): void {
  registrationState(ts);
  ts.publisherSlotRegistrationVersion = 1;
  ts.registerPublisherSlot = (
    options: PublisherSlotRegistrationOptions
  ): PublisherSlotRegistration => {
    const state = registrationState(ts);
    const rejected = (status: PublisherSlotRegistrationStatus): PublisherSlotRegistration => ({
      getStatus: () => ({ ...status }),
      cancel: () => {},
    });
    if (
      !options ||
      typeof options.slotId !== 'string' ||
      !options.slotId ||
      !Number.isInteger(options.navigationGeneration) ||
      !Array.isArray(options.eligibleFormats) ||
      !options.eligibleFormats.every(finiteSize) ||
      !options.gptSlot ||
      !['getSlotElementId', 'getAdUnitPath', 'setTargeting', 'clearTargeting'].every(
        (key) => typeof (options.gptSlot as unknown as Record<string, unknown>)[key] === 'function'
      )
    ) {
      return rejected({ status: 'rejected', reason: 'invalid_arguments' });
    }
    if (options.navigationGeneration !== state.generation)
      return rejected({ status: 'rejected', reason: 'stale_generation' });
    const tag = registrationTag();
    const policy = loadPolicy(ts, tag);
    if (policy) return rejected(policy);
    let elementId: string;
    let adUnitPath: string;
    try {
      elementId = options.gptSlot.getSlotElementId();
      adUnitPath = options.gptSlot.getAdUnitPath();
      if (typeof elementId !== 'string' || typeof adUnitPath !== 'string')
        throw new Error('invalid slot identity');
      if (!tag?.pubads?.().getSlots?.().includes(options.gptSlot))
        return rejected({ status: 'rejected', reason: 'native_slot_missing' });
    } catch {
      return rejected({ status: 'rejected', reason: 'invalid_arguments' });
    }
    const { element, error } = exactElement(elementId);
    if (error || !element) return rejected(error!);
    const configured = state.payload?.slots.find((slot) => slot.id === options.slotId);
    if (configured && configured.gam_unit_path !== adUnitPath) {
      return rejected({ status: 'rejected', reason: 'path_mismatch' });
    }
    const eligibleFormats = normalizedFormats(options.eligibleFormats);
    const previous = state.records[options.slotId];
    if (previous && (previous.consumed || previous.status.status !== 'cancelled')) {
      if (
        previous.gptSlot === options.gptSlot &&
        previous.element === element &&
        JSON.stringify(previous.eligibleFormats) === JSON.stringify(eligibleFormats)
      )
        return registrationHandle(ts, previous);
      return rejected({ status: 'rejected', reason: 'conflicting_registration' });
    }
    const record: PublisherSlotRegistrationRecord = {
      generation: state.generation,
      slotId: options.slotId,
      token: ++state.nextToken,
      gptSlot: options.gptSlot,
      element,
      elementId,
      adUnitPath,
      eligibleFormats,
      status: { status: 'waiting_payload' },
      consumed: false,
      queued: false,
    };
    state.records[record.slotId] = record;
    ts.publisherSlotRegistrationDispatch?.();
    return registrationHandle(ts, record);
  };

  // Every queued callback resolves this shared function, including callbacks created by the bootstrap.
  ts.publisherSlotRegistrationDispatch = (readyRecord?: PublisherSlotRegistrationRecord): void => {
    const state = registrationState(ts);
    const current = (record: PublisherSlotRegistrationRecord) =>
      state.records[record.slotId] === record &&
      record.generation === state.generation &&
      !record.consumed &&
      (record.status.status === 'waiting_payload' || record.status.status === 'waiting_delivery');
    const dispatch = (record: PublisherSlotRegistrationRecord): void => {
      if (!current(record)) return;
      record.queued = false;
      const payload = state.payload;
      if (!payload || payload.generation !== state.generation) return;
      const slot = payload.slots.find((candidate) => candidate.id === record.slotId);
      if (!slot || slot.initialization !== 'publisher_registered') {
        record.status = {
          status: 'inactive',
          reason: slot ? 'not_registered_mode' : 'not_admitted',
        };
        return;
      }
      const tag = registrationTag();
      const policy = loadPolicy(ts, tag);
      if (policy) {
        record.status = policy;
        return;
      }
      const identity = exactElement(record.elementId);
      if (
        !record.element ||
        identity.element !== record.element ||
        record.element.id !== record.elementId
      ) {
        record.status = { status: 'cancelled', reason: 'element_changed' };
        return;
      }
      const native = record.gptSlot;
      const service = tag?.pubads?.();
      if (
        !native ||
        !service?.getSlots?.().includes(native) ||
        native.getSlotElementId() !== record.elementId
      ) {
        record.status = { status: 'cancelled', reason: 'native_slot_missing' };
        return;
      }
      if (
        native.getAdUnitPath() !== record.adUnitPath ||
        record.adUnitPath !== slot.gam_unit_path
      ) {
        record.status = { status: 'rejected', reason: 'path_mismatch' };
        return;
      }
      if (document.hidden) {
        queue(record);
        return;
      }
      // The existing claim registry and the logical consumed marker have different lifetimes.
      const first = claim(record.element);
      if (!first) {
        record.status = { status: 'publisher_owned', reason: 'prior_claim' };
        return;
      }
      record.consumed = true;
      record.status = { status: 'dispatching' };
      const wasInternal = ts.gptSlotHandoffInternal;
      const wasRefreshing = ts.adInitRefreshInProgress;
      try {
        const bid = payload.bids[record.slotId];
        const fits =
          bid &&
          finiteSize([bid.w, bid.h]) &&
          slot.formats.some((size) => size[0] === bid.w && size[1] === bid.h) &&
          record.eligibleFormats.some((size) => size[0] === bid.w && size[1] === bid.h);
        const candidate = ts.tsRenderBridgeVersion === 1 && fits ? bid : undefined;
        record.selectedCandidate = candidate;
        if (!candidate)
          record.noCandidateReason = !bid
            ? 'no_bid'
            : ts.tsRenderBridgeVersion !== 1
              ? 'renderer_unavailable'
              : 'incompatible_dimensions';
        const previousKeys = ts.prevSlotTargetingKeys?.[record.elementId] ?? [];
        for (const key of new Set([...CANDIDATE_KEYS, 'ts_initial', ...previousKeys]))
          native.clearTargeting(key);
        const targeting: Record<string, string | string[]> = { ...slot.targeting, ts_initial: '1' };
        // Configured hb_* values are also candidate keys, never a back door around eligibility.
        for (const key of CANDIDATE_KEYS) {
          delete targeting[key];
          if (candidate?.[key]) targeting[key] = String(candidate[key]);
        }
        record.targetingKeys = Object.keys(targeting);
        for (const [key, value] of Object.entries(targeting)) native.setTargeting(key, value);
        first.targeting = targeting;
        ts.divToSlotId = Object.assign(Object.create(null), ts.divToSlotId);
        ts.prevSlotTargetingKeys = Object.assign(Object.create(null), ts.prevSlotTargetingKeys);
        retainPublisherSlotMappings(ts, ts.divToSlotId, ts.prevSlotTargetingKeys);
        try {
          ts.gptDiagnosticsRecorder?.recordTrustedServerOpportunity(
            native,
            slot.id,
            candidate?.hb_adid
              ? candidate.adm || candidate.hb_cache_host
                ? 'renderable_candidate'
                : 'unrenderable_candidate'
              : 'no_candidate',
            candidate?.hb_auction_id,
            record.eligibleFormats
          );
        } catch {
          /* Diagnostics never own delivery. */
        }
        ts.gptSlotHandoffInternal = true;
        ts.adInitRefreshInProgress = true;
        tag!.display!(record.elementId);
        service.refresh([native]);
        if (record.status.status === 'dispatching') record.status = { status: 'requested' };
      } catch {
        record.status = { status: 'failed_after_dispatch', reason: 'delivery_exception' };
      } finally {
        ts.gptSlotHandoffInternal = wasInternal;
        ts.adInitRefreshInProgress = wasRefreshing;
      }
    };
    const queue = (record: PublisherSlotRegistrationRecord): void => {
      if (!current(record) || record.queued) return;
      record.queued = true;
      record.status = { status: 'waiting_delivery' };
      const foreground = (): void => {
        if (!current(record)) return;
        if (document.hidden) {
          document.addEventListener('visibilitychange', foreground, { once: true });
          return;
        }
        window.requestAnimationFrame(() => {
          if (!current(record)) return;
          if (document.hidden) {
            foreground();
            return;
          }
          registrationTag()?.cmd?.push(() => ts.publisherSlotRegistrationDispatch?.(record));
        });
      };
      foreground();
    };
    if (readyRecord) {
      dispatch(readyRecord);
      return;
    }
    for (const record of Object.values(state.records)) {
      if (!current(record)) continue;
      if (!state.payload) {
        record.status = { status: 'waiting_payload' };
        continue;
      }
      const admitted = state.payload.slots.find((slot) => slot.id === record.slotId);
      if (!admitted || admitted.initialization !== 'publisher_registered') {
        record.status = {
          status: 'inactive',
          reason: admitted ? 'not_registered_mode' : 'not_admitted',
        };
        continue;
      }
      queue(record);
    }
  };
}
