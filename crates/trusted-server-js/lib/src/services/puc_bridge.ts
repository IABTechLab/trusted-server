import {
  TSJS_MESSAGE_PROTOCOL_V1,
  type MessagingAdapter,
  type MessagingPort,
} from '../adapters/messaging';

import type { ReservationRecognition, ReservationService } from './reservations';

const mapGetIntrinsic = Map.prototype.get;
const mapSetIntrinsic = Map.prototype.set;
const mapClearIntrinsic = Map.prototype.clear;
const mapSizeGetter = Object.getOwnPropertyDescriptor(Map.prototype, 'size')?.get as (
  this: Map<unknown, unknown>
) => number;
const mapValuesIntrinsic = Map.prototype.values;
const mapIteratorNextIntrinsic = Object.getPrototypeOf(new Map().values()).next as (
  this: IterableIterator<unknown>
) => IteratorResult<unknown>;
const jsonStringifyIntrinsic = JSON.stringify;
const objectFreezeIntrinsic = Object.freeze;

interface PendingClaim {
  readonly port: MessagingPort;
  readonly source: object;
}

export interface PucBridgeOptions {
  readonly messaging: MessagingAdapter;
  readonly reservations: Pick<ReservationService, 'recognize'>;
}

export interface PucBridgeInventory {
  readonly disposed: boolean;
  readonly pendingClaims: number;
}

export interface PucBridge {
  dispose(): void;
  snapshotInventoryForTest(): PucBridgeInventory;
}

function mapValue<Key, Value>(map: Map<Key, Value>, key: Key): Value | undefined {
  return Reflect.apply(mapGetIntrinsic, map, [key]) as Value | undefined;
}

function setMapValue<Key, Value>(map: Map<Key, Value>, key: Key, value: Value): void {
  Reflect.apply(mapSetIntrinsic, map, [key, value]);
}

function mapSize<Key, Value>(map: Map<Key, Value>): number {
  return Reflect.apply(mapSizeGetter, map, []) as number;
}

function snapshotMapValues<Key, Value>(map: Map<Key, Value>): readonly Value[] {
  const iterator = Reflect.apply(mapValuesIntrinsic, map, []) as IterableIterator<Value>;
  const values: Value[] = [];
  while (true) {
    const step = Reflect.apply(mapIteratorNextIntrinsic, iterator, []) as IteratorResult<Value>;
    if (step.done) return values;
    values[values.length] = step.value;
  }
}

function frozen<Value extends object>(value: Value): Readonly<Value> {
  return Reflect.apply(objectFreezeIntrinsic, Object, [value]) as Readonly<Value>;
}

function recognizedReservation(
  reservations: Pick<ReservationService, 'recognize'>,
  reservationId: string
): ReservationRecognition | undefined {
  try {
    return reservations.recognize(reservationId);
  } catch {
    return undefined;
  }
}

function suppress(event: unknown): boolean {
  try {
    if (typeof event !== 'object' || event === null) return false;
    const stop = Reflect.get(event, 'stopImmediatePropagation');
    if (typeof stop !== 'function') return false;
    Reflect.apply(stop, event, []);
    return true;
  } catch {
    return false;
  }
}

function eventData(event: unknown): unknown {
  try {
    return typeof event === 'object' && event !== null ? Reflect.get(event, 'data') : undefined;
  } catch {
    return undefined;
  }
}

function eventSource(event: unknown): object | undefined {
  try {
    if (typeof event !== 'object' || event === null) return undefined;
    const source = Reflect.get(event, 'source');
    return (typeof source === 'object' || typeof source === 'function') && source !== null
      ? source
      : undefined;
  } catch {
    return undefined;
  }
}

function refusedResponse(adId: string): string | undefined {
  try {
    const owner = Object.create(null) as Record<string, unknown>;
    owner['version'] = 1;
    owner['status'] = TSJS_MESSAGE_PROTOCOL_V1.status.refused;
    const response = Object.create(null) as Record<string, unknown>;
    response['message'] = TSJS_MESSAGE_PROTOCOL_V1.message.prebidResponse;
    response['adId'] = adId;
    response['rendererVersion'] = TSJS_MESSAGE_PROTOCOL_V1.rendererVersion;
    response['tsOwner'] = owner;
    const serialized = Reflect.apply(jsonStringifyIntrinsic, JSON, [response]) as unknown;
    return typeof serialized === 'string' ? serialized : undefined;
  } catch {
    return undefined;
  }
}

function refuse(port: MessagingPort, adId: string): void {
  try {
    const response = refusedResponse(adId);
    if (response !== undefined) port.post(response, []);
  } catch {
    // Refusal transport is best-effort; endpoint closure remains mandatory.
  } finally {
    try {
      port.close();
    } catch {
      // The adapter contains raw close failures, but keep this boundary fail-closed.
    }
  }
}

/**
 * Own the runtime-wide Universal Creative capture dispatcher.
 *
 * Request recognition deliberately precedes exact parsing and port inspection so
 * malformed or replayed TS capabilities cannot fall through to native Prebid.
 */
export function createPucBridge(options: PucBridgeOptions): PucBridge {
  const messaging = options.messaging;
  const reservations = options.reservations;
  const pendingClaims = new Map<string, PendingClaim>();
  let disposed = false;

  const dispatch = (event: MessageEvent): void => {
    if (disposed) return;
    const data = eventData(event);
    const routing = messaging.inspectGlobalMessage(data);
    if (
      routing?.message !== TSJS_MESSAGE_PROTOCOL_V1.message.prebidRequest ||
      routing.adId === undefined
    ) {
      return;
    }

    const recognition = recognizedReservation(reservations, routing.adId);
    if (recognition?.recognized !== true) return;
    if (!suppress(event)) return;

    const exact = messaging.parseProtocolMessage('prebidRequest', data);
    const ports = messaging.extractTransferredPorts(event, 1);
    const port = ports?.[0];
    if (!port) return;
    if (exact === undefined || recognition.state !== 'renderable') {
      refuse(port, routing.adId);
      return;
    }

    const source = eventSource(event);
    if (source === undefined || mapValue(pendingClaims, routing.adId) !== undefined) {
      refuse(port, routing.adId);
      return;
    }

    setMapValue(pendingClaims, routing.adId, frozen({ port, source }));
  };

  const uninstall = messaging.installCaptureListener(dispatch);

  const bridge: PucBridge = {
    dispose(): void {
      if (disposed) return;
      disposed = true;
      try {
        uninstall();
      } catch {
        // Listener removal is already contained by the adapter.
      }
      const claims = snapshotMapValues(pendingClaims);
      for (let index = 0; index < claims.length; index += 1) {
        const claim = claims[index];
        if (!claim) continue;
        try {
          claim.port.close();
        } catch {
          // Endpoint cleanup is exact-once at the adapter facade.
        }
      }
      Reflect.apply(mapClearIntrinsic, pendingClaims, []);
    },
    snapshotInventoryForTest(): PucBridgeInventory {
      return frozen({ disposed, pendingClaims: mapSize(pendingClaims) });
    },
  };
  return frozen(bridge);
}
