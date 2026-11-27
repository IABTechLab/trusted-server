import type { TsjsApi } from './types';

type Targeting = Record<string, string | string[]>;
type Scope = string | string[] | undefined;
type NativeBid = {
  adId: string;
  adUnitCode: string;
  adapterCode?: string;
  bidderCode: string;
  cpm: number;
  dealId?: string;
  sendStandardTargeting?: boolean;
  adserverTargeting: Targeting;
  meta?: Record<string, unknown>;
};
type Slot = {
  getSlotElementId(): string;
  getAdUnitPath(): string;
  getTargeting(key: string): string[];
  setTargeting(key: string, value: string | string[]): unknown;
  clearTargeting(key: string): unknown;
};
type Prebid = {
  getConfig(key: string): unknown;
  onEvent(event: string, listener: (value: unknown) => void): void;
  getBidResponsesForAdUnitCode(code: string): { bids: NativeBid[] };
  getAdserverTargeting(scope?: Scope): Record<string, Targeting>;
  getAdserverTargetingForAdUnitCode(code: string): Targeting | undefined;
  getAdserverTargetingForAdUnitCodeStr(code: string): string | undefined;
  setTargetingForGPTAsync(...args: unknown[]): unknown;
};
type SelectedTargeting = { generation: number; values: Record<string, string> };
type BoundTargeting = SelectedTargeting & {
  adId: string;
  adUnitCode: string;
  bidderCode: string;
  nativeTargeting: Targeting;
  unitKeys: Set<string>;
  refused?: boolean;
};
type Frame = {
  previous?: Frame;
  closed: boolean;
  consumed: boolean;
  receipts: Map<Slot, Set<string>>;
};
type DeliveryOwner = { key: string; values: string[] };
type AppliedKey = {
  applied: string[];
  baseline: string[];
  fromNative: boolean;
  owner?: DeliveryOwner;
};

/** Internal bridge installed by core before the generated external Prebid bundle drains queues. */
export interface PrebidTargetingBridge {
  /** Install against a fresh, controlled Prebid instance before publisher callbacks run. */
  install(prebid: unknown): void;
  /** Restore managed GPT keys before navigation or a replacement refresh. */
  clear(slots: readonly unknown[]): void;
  /** Admit an optional finalized map only when the early delivery bridge is available. */
  admit(prebid: unknown, code: string, values: unknown, generation?: number): unknown;
}

function hasOwn(value: object, key: PropertyKey): boolean {
  return Object.prototype.hasOwnProperty.call(value, key);
}

const CARRIER = 'tsjsSelectedTargeting';
const HOST_KEYS = new Set(['hb_adid', 'hb_cache_host', 'hb_cache_path', 'ts_initial']);
// Public native delivery fields, including configurations with no bidder suffix
// receipts. Engine keys remain dynamic; free-form native custom aggregates stay
// under Prebid's control. This list does not include global hb_ver or ts_initial.
const NATIVE_DELIVERY_KEYS = new Set([
  'hb_adid',
  'hb_pb',
  'hb_bidder',
  'hb_size',
  'hb_deal',
  'hb_source',
  'hb_format',
  'hb_uuid',
  'hb_cache_id',
  'hb_cache_host',
  'hb_cache_path',
  'hb_adomain',
  'hb_acat',
  'hb_crid',
  'hb_dsp',
]);

const NATIVE_DELIVERY_PREFIXES = [...NATIVE_DELIVERY_KEYS].map((key) => `${key}_`);

function nativeValues(value: string | string[]): string[] {
  return String(value).split(',');
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function selectedValues(value: unknown): Record<string, string> | undefined {
  if (!isRecord(value)) return;
  const entries = Object.entries(value);
  if (entries.length > 128) return;
  let characters = 0;
  for (const [key, item] of entries) {
    if (!key || key.length > 256 || typeof item !== 'string' || item.length > 8192) return;
    characters += key.length + item.length;
    if (characters > 65536) return;
  }
  // Own data properties preserve keys such as __proto__ without invoking setters.
  return Object.fromEntries(entries) as Record<string, string>;
}

function hasAmbiguousNativeKeys(targeting: Targeting): boolean {
  const keys = new Set(Object.keys(targeting));
  for (const key of keys) {
    for (let index = key.indexOf('_'); index >= 0; index = key.indexOf('_', index + 1)) {
      if (keys.has(key.slice(0, index))) return true;
    }
  }
  return false;
}

function sameValues(left: string[], right: string[]): boolean {
  return left.length === right.length && left.every((item, index) => item === right[index]);
}

/**
 * Create the public Prebid delivery bridge.
 *
 * Prebid owns bid eligibility, caching and selection. We resolve only the exact
 * unsuffixed native render ID in its returned targeting, then overlay that bid's
 * finalized map. Weak associations retain no bids beyond Prebid's own lifetime.
 */
export function createPrebidTargetingBridge(api: TsjsApi): PrebidTargetingBridge {
  const instances = new WeakMap<object, { admit: PrebidTargetingBridge['admit'] }>();
  const applied = new WeakMap<Slot, Map<string, AppliedKey>>();
  function restore(slot: Slot, incoming: Targeting): void {
    const previous = applied.get(slot);
    // Read each owner before clearing any key, including the owner key itself.
    // A publisher may have installed a different bid with the same price.
    const owners = new Map<string, string[]>();
    for (const state of previous?.values() ?? []) {
      if (state.owner && !owners.has(state.owner.key)) {
        owners.set(state.owner.key, [...slot.getTargeting(state.owner.key)]);
      }
    }
    for (const [key, state] of previous ?? []) {
      if (state.owner && !sameValues(owners.get(state.owner.key) ?? [], state.owner.values))
        continue;
      if (hasOwn(incoming, key) || !sameValues(slot.getTargeting(key), state.applied)) continue;
      if (!state.fromNative && state.baseline.length) slot.setTargeting(key, state.baseline);
      else slot.clearTargeting(key);
    }
    applied.delete(slot);
  }
  return {
    clear(slots) {
      for (const slot of slots) {
        if (isRecord(slot) && applied.has(slot as unknown as Slot))
          restore(slot as unknown as Slot, {});
      }
    },
    install(value) {
      if (!isRecord(value) || instances.has(value)) return;
      const names = [
        'getConfig',
        'onEvent',
        'getBidResponsesForAdUnitCode',
        'getAdserverTargeting',
        'getAdserverTargetingForAdUnitCode',
        'getAdserverTargetingForAdUnitCodeStr',
        'setTargetingForGPTAsync',
      ];
      if (names.some((name) => typeof value[name] !== 'function')) return;
      const prebid = value as unknown as Prebid;
      const records = new WeakMap<NativeBid, BoundTargeting>();
      const units = new Set<string>();
      const unsupportedUnits = new Set<string>();
      let unsupportedCharacters = 0;
      let unsupportedOverflow = false;
      let unitCharacters = 0;
      let generation = api.navGeneration ?? 0;
      let hasManagedSlots = false;
      let active: Frame | undefined;
      const nativeAll = prebid.getAdserverTargeting;
      const nativeScoped = prebid.getAdserverTargetingForAdUnitCode;
      const nativeString = prebid.getAdserverTargetingForAdUnitCodeStr;
      const nativeGpt = prebid.setTargetingForGPTAsync;
      const cachedBids = prebid.getBidResponsesForAdUnitCode.bind(prebid);

      function inspectUnits(value: unknown): void {
        if (!isRecord(value) || !Array.isArray(value.adUnits)) return;
        for (const unit of value.adUnits) {
          if (
            !isRecord(unit) ||
            typeof unit.code !== 'string' ||
            !isRecord(unit.adserverTargeting) ||
            !hasOwn(unit.adserverTargeting, 'hb_adid') ||
            unsupportedUnits.has(unit.code)
          )
            continue;
          // Native unit targeting is merged after bid targeting, including units
          // retained from earlier auctions. Never evict a known unsafe code.
          if (unsupportedUnits.size >= 1024 || unsupportedCharacters + unit.code.length > 65536) {
            unsupportedOverflow = true;
          } else {
            unsupportedUnits.add(unit.code);
            unsupportedCharacters += unit.code.length;
          }
        }
      }
      prebid.onEvent('auctionInit', inspectUnits);
      prebid.onEvent('auctionEnd', (value) => {
        inspectUnits(value);
        // A pending auction is absent from the public cached-bid getter. Its
        // completion must recheck records even after navigation clears the index.
        if (!isRecord(value) || !Array.isArray(value.bidsReceived)) return;
        for (const bid of value.bidsReceived as NativeBid[]) {
          const record = records.get(bid);
          if (!record) continue;
          if (Array.isArray(value.adUnits)) {
            for (const unit of value.adUnits) {
              if (
                isRecord(unit) &&
                unit.code === bid.adUnitCode &&
                isRecord(unit.adserverTargeting)
              ) {
                for (const key of Object.keys(unit.adserverTargeting)) record.unitKeys.add(key);
              }
            }
          }
          if (!compatible(bid, record)) refuse(bid);
        }
      });

      function identityAvailable(): boolean {
        const allowed = prebid.getConfig('targetingControls.allowTargetingKeys');
        return (
          !(Number(prebid.getConfig('targetingControls.auctionKeyMaxChars')) > 0) &&
          (!Array.isArray(allowed) || !allowed.length || allowed.includes('AD_ID'))
        );
      }
      function compatible(bid: NativeBid, record: BoundTargeting): boolean {
        if (
          record.refused ||
          // A custom base such as hb can collide with standard suffixed keys.
          // Their ownership cannot be separated after native flattening.
          hasAmbiguousNativeKeys(record.nativeTargeting) ||
          unsupportedOverflow ||
          unsupportedUnits.has(bid.adUnitCode) ||
          record.generation !== (api.navGeneration ?? 0) ||
          !identityAvailable() ||
          bid.sendStandardTargeting === false ||
          bid.adId !== record.adId ||
          bid.adserverTargeting?.hb_adid !== record.adId
        )
          return false;
        const lock = prebid.getConfig('targetingControls.lock');
        const locks = Array.isArray(lock) ? lock : lock == null ? [] : [lock];
        // Native locks precede the public event; render completion reads the
        // delivered values. Changing a locked value would leave those unequal.
        return !locks.some(
          (key) =>
            typeof key === 'string' &&
            !HOST_KEYS.has(key) &&
            hasOwn(record.values, key) &&
            record.values[key] !== bid.adserverTargeting[key]
        );
      }
      function refuse(bid: NativeBid): void {
        bid.cpm = -1;
        // A preferred deal can otherwise bypass the native positive-CPM filter.
        delete bid.dealId;
        bid.adserverTargeting = {};
        // Preserve a weak tombstone: an outer GPT invocation may already hold
        // this bid's native targeting while a nested getter refuses the bid.
        const record = records.get(bid);
        if (record) record.refused = true;
      }
      function prepare(): void {
        for (const unit of units) {
          for (const bid of cachedBids(unit).bids) {
            const record = records.get(bid);
            if (record && !compatible(bid, record)) refuse(bid);
          }
        }
        if (generation !== (api.navGeneration ?? 0)) {
          units.clear();
          unitCharacters = 0;
          generation = api.navGeneration ?? 0;
        }
      }
      function owner(
        unit: string,
        targeting: Targeting
      ): { bid: NativeBid; record: BoundTargeting } | undefined {
        const id = targeting.hb_adid;
        if (typeof id !== 'string') return;
        const matches = cachedBids(unit).bids.filter(
          (bid) => bid.adId === id || records.get(bid)?.adId === id
        );
        if (matches.length !== 1) return;
        const record = records.get(matches[0]);
        return record ? { bid: matches[0], record } : undefined;
      }
      function withoutOwnedTargeting(targeting: Targeting, record: BoundTargeting): Targeting {
        // Native suffixed keys may truncate the bidder suffix differently for
        // each base key. Associate observed suffixes with their native render ID;
        // shared prefixes stay native unless an exact field is uniquely owned.
        const identities = Object.entries(targeting)
          .filter(([key]) => key.startsWith('hb_adid_'))
          .map(([key, item]) => ({
            suffix: key.slice('hb_adid_'.length),
            truncated: key.length === 20,
            owned: Array.isArray(item) ? item.includes(record.adId) : item === record.adId,
          }));
        const nativeKeys = Object.keys(record.nativeTargeting);
        const otherBidders = cachedBids(record.adUnitCode).bids.filter(
          (bid) => bid.bidderCode !== record.bidderCode
        );
        const dealKey = `hb_deal_${record.bidderCode}`.slice(0, 20);
        const bidderIdKey = `hb_adid_${record.bidderCode}`.slice(0, 20);
        return Object.fromEntries(
          Object.entries(targeting).filter(([key, item]) => {
            const ownedSuffix = nativeKeys.some((base) => {
              if (!key.startsWith(`${base}_`)) return false;
              const suffix = key.slice(base.length + 1);
              const exact = identities.filter((identity) => identity.suffix === suffix);
              // An exact foreign ID receipt takes precedence over a shared
              // prefix. Prefix matching is only valid at the pinned native
              // 20-character boundary already qualified by artifact controls.
              const matches = exact.length
                ? exact
                : identities.filter(
                    (identity) =>
                      (key.length === 20 && identity.suffix.startsWith(suffix)) ||
                      (identity.truncated && suffix.startsWith(identity.suffix))
                  );
              const standardKey = NATIVE_DELIVERY_KEYS.has(base);
              const exactNative =
                key === `${base}_${record.bidderCode}`.slice(0, 20) &&
                !otherBidders.some((bid) => key === `${base}_${bid.bidderCode}`.slice(0, 20)) &&
                sameValues(nativeValues(item), nativeValues(record.nativeTargeting[base]));
              // An owned, collapsed ID prefix does not claim a foreign bidder's
              // longer price namespace or a shared shorter metadata field.
              if (standardKey && !exactNative) return false;
              if (matches.length > 0 && matches.every((identity) => identity.owned)) return true;
              // A shorter render-ID namespace can collapse while a price key
              // still identifies this bidder uniquely. Retain shared fields;
              // only claim the exact native value under an unshared standard key.
              return standardKey && exactNative;
            });
            const ownedKey = hasOwn(record.nativeTargeting, key) && !record.unitKeys.has(key);
            const renderIdentity = Array.isArray(item)
              ? item.includes(record.adId)
              : item === record.adId;
            // Native winning-deal targeting includes a bidder key even when
            // sendAll is off and no suffixed render-ID receipt was emitted.
            const winningDeal =
              key === dealKey &&
              !hasOwn(targeting, bidderIdKey) &&
              hasOwn(record.nativeTargeting, 'hb_deal') &&
              sameValues(nativeValues(item), nativeValues(record.nativeTargeting.hb_deal));
            return !ownedKey && !ownedSuffix && !renderIdentity && !winningDeal;
          })
        );
      }
      function project(unit: string, targeting: Targeting): Targeting {
        const owned = owner(unit, targeting);
        if (!owned) return targeting;
        if (!compatible(owned.bid, owned.record)) {
          refuse(owned.bid);
          return withoutOwnedTargeting(targeting, owned.record);
        }
        const record = owned.record;
        return {
          ...targeting,
          ...Object.fromEntries(
            Object.entries(record.values).filter(([key]) => !HOST_KEYS.has(key))
          ),
        };
      }
      function close(frame: Frame): void {
        frame.closed = true;
        frame.receipts.clear();
        if (active === frame) {
          active = frame.previous;
          while (active?.closed) active = active.previous;
        }
      }
      function activate(frame: Frame): void {
        if (active !== frame) {
          frame.previous = active;
          frame.closed = false;
          active = frame;
          // A public targeting hook may defer its continuation until after the
          // wrapper returns. Matching reopens that invocation for this turn.
          queueMicrotask(() => close(frame));
        }
      }
      function deliver(slot: Slot, incoming: Targeting, record?: BoundTargeting): void {
        restore(slot, incoming);
        if (!record) return;
        const next = new Map<string, AppliedKey>();
        const canonicalOwner: DeliveryOwner = {
          key: 'hb_adid',
          values: [...slot.getTargeting('hb_adid')],
        };
        const bidderIdKey = `hb_adid_${record.bidderCode}`.slice(0, 20);
        const bidderOwner =
          hasOwn(incoming, bidderIdKey) &&
          sameValues(nativeValues(incoming[bidderIdKey]), [record.adId])
            ? { key: bidderIdKey, values: [...slot.getTargeting(bidderIdKey)] }
            : canonicalOwner;
        const retained = withoutOwnedTargeting(incoming, record);
        for (const key of Object.keys(incoming)) {
          if (hasOwn(retained, key)) continue;
          const canonical = NATIVE_DELIVERY_KEYS.has(key);
          const bidderKey = NATIVE_DELIVERY_PREFIXES.some((prefix) => key.startsWith(prefix));
          const values = [...slot.getTargeting(key)];
          const renderIdentity = sameValues(values, [record.adId]);
          if (!canonical && !bidderKey && !renderIdentity) continue;
          next.set(key, {
            applied: values,
            baseline: [],
            fromNative: true,
            owner: bidderKey ? bidderOwner : canonicalOwner,
          });
        }
        for (const [key, item] of Object.entries(record.values)) {
          if (HOST_KEYS.has(key)) continue;
          const baseline = [...slot.getTargeting(key)];
          // The finalized carrier is a string map, not a flattened native list.
          const values = [item];
          slot.setTargeting(key, values);
          const native = next.get(key);
          next.set(key, {
            applied: values,
            baseline,
            fromNative: native !== undefined,
            owner: native?.owner ?? (NATIVE_DELIVERY_KEYS.has(key) ? canonicalOwner : undefined),
          });
        }
        // The receipt describes the final values, including any explicit engine
        // overlay. It never extends native bid ownership or changes selection.
        bidderOwner.values = [...slot.getTargeting(bidderOwner.key)];
        applied.set(slot, next);
        hasManagedSlots = true;
      }

      // Registered by a side-effect import immediately after Prebid's entry,
      // before selected modules and the publisher queue can add listeners.
      prebid.onEvent('bidResponse', (value) => {
        if (!isRecord(value)) return;
        const bid = value as unknown as NativeBid;
        const carrier = bid.meta && hasOwn(bid.meta, CARRIER) ? bid.meta[CARRIER] : undefined;
        if (bid.adapterCode !== 'trustedServer' || !isRecord(carrier)) return;
        delete bid.meta?.[CARRIER];
        const values = selectedValues(carrier.values);
        // Render identity belongs to this host. Publisher settings cannot replace
        // it for a targeting-dependent local candidate.
        bid.adserverTargeting = { ...bid.adserverTargeting, hb_adid: bid.adId };
        const record: BoundTargeting = {
          generation: carrier.generation as number,
          values: values ?? {},
          adId: bid.adId,
          adUnitCode: bid.adUnitCode,
          bidderCode: bid.bidderCode,
          nativeTargeting: bid.adserverTargeting,
          unitKeys: new Set(),
        };
        if (!values || !units.has(bid.adUnitCode) || !compatible(bid, record)) {
          refuse(bid);
          return;
        }
        records.set(bid, record);
      });
      prebid.onEvent('setTargeting', (event) => {
        if (!active || active.consumed || !isRecord(event)) return;
        const frame = active;
        // Keep a consumed sentinel until the native invocation returns. A later
        // listener can emit AST events or nest a GPT invocation of its own.
        frame.consumed = true;
        for (const [unit, incoming] of Object.entries(event)) {
          if (!isRecord(incoming)) continue;
          const targeting = incoming as Targeting;
          const owned = owner(unit, targeting);
          const rejected = owned !== undefined && !compatible(owned.bid, owned.record);
          const record = rejected ? undefined : owned?.record;
          const retained = rejected ? withoutOwnedTargeting(targeting, owned.record) : targeting;
          if (rejected) refuse(owned.bid);
          for (const [slot, codes] of frame.receipts) {
            if ([...codes].pop() !== unit) continue;
            const id = targeting.hb_adid;
            const deliveredId = slot.getTargeting('hb_adid');
            if (record && (typeof id !== 'string' || !sameValues(deliveredId, [id]))) continue;
            if (rejected) {
              // A publisher matching callback can change prerequisites after
              // native selection. Clear only values this native attempt wrote;
              // never fall back to its recomputed price for this local bid.
              for (const [key, item] of Object.entries(targeting)) {
                if (hasOwn(retained, key)) continue;
                const values = Array.isArray(item) ? item : [item];
                if (sameValues(slot.getTargeting(key), values)) slot.clearTargeting(key);
              }
            }
            deliver(slot, retained, record);
          }
          // Use a new unit map: the original native map can belong to locks.
          event[unit] = rejected ? retained : project(unit, targeting);
        }
        frame.receipts.clear();
      });

      prebid.getAdserverTargeting = function (...args) {
        prepare();
        const native = nativeAll.apply(this, args);
        return Object.fromEntries(
          Object.entries(native).map(([unit, targeting]) => [unit, project(unit, targeting)])
        );
      };
      prebid.getAdserverTargetingForAdUnitCode = function (code) {
        prepare();
        const native = nativeScoped.call(this, code);
        return native ? project(code, native) : native;
      };
      prebid.getAdserverTargetingForAdUnitCodeStr = function (code) {
        prepare();
        if (!code || typeof code !== 'string') return nativeString.call(this, code);
        const native = nativeScoped.call(this, code);
        // Pinned Prebid's exact encoder: keys are literal, values are encoded.
        return native
          ? Object.entries(project(code, native))
              .map(([key, item]) => `${key}=${encodeURIComponent(String(item))}`)
              .join('&')
          : nativeString.call(this, code);
      };
      prebid.setTargetingForGPTAsync = function (...args) {
        prepare();
        if (!units.size && !hasManagedSlots) return nativeGpt.apply(this, args);
        const frame: Frame = {
          previous: active,
          closed: false,
          consumed: false,
          receipts: new Map(),
        };
        const matching = args[1];
        args[1] = function (this: unknown, slot: Slot) {
          activate(frame);
          let custom: unknown;
          active = undefined;
          try {
            custom = typeof matching === 'function' ? matching.call(this, slot) : undefined;
          } finally {
            active = frame;
          }
          return function (this: unknown, ...predicateArgs: [string, number, string[]]) {
            const unit = predicateArgs[0];
            let matched: unknown;
            active = undefined;
            try {
              matched =
                typeof custom === 'function'
                  ? custom.apply(this, predicateArgs)
                  : slot.getAdUnitPath() === unit || slot.getSlotElementId() === unit;
            } finally {
              active = frame;
            }
            if (matched) {
              const codes = frame.receipts.get(slot) ?? new Set<string>();
              codes.add(unit);
              frame.receipts.set(slot, codes);
            }
            return matched;
          };
        };
        activate(frame);
        try {
          return nativeGpt.apply(this, args);
        } finally {
          close(frame);
        }
      };
      instances.set(value, {
        admit(_prebid, unit, values, requestGeneration) {
          if (generation !== (api.navGeneration ?? 0)) prepare();
          const map = selectedValues(values);
          if (!map || requestGeneration !== (api.navGeneration ?? 0) || !identityAvailable())
            return;
          if (!units.has(unit)) {
            if (!unit || units.size >= 1024 || unitCharacters + unit.length > 65536) return;
            units.add(unit);
            unitCharacters += unit.length;
          }
          return { generation: requestGeneration, values: map } satisfies SelectedTargeting;
        },
      });
    },
    admit(prebid, code, values, generation) {
      return isRecord(prebid)
        ? instances.get(prebid)?.admit(prebid, code, values, generation)
        : undefined;
    },
  };
}
