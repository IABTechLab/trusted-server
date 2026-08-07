const MAX_GLOBAL_MESSAGE_BYTES = 4_096;
const setDeleteIntrinsic = Set.prototype.delete;
const setValuesIntrinsic = Set.prototype.values;
const setIteratorNextIntrinsic = Reflect.get(
  Object.getPrototypeOf(Reflect.apply(setValuesIntrinsic, new Set(), [])),
  'next'
) as (...arguments_: unknown[]) => unknown;
const weakMapGetIntrinsic = WeakMap.prototype.get;
const weakMapSetIntrinsic = WeakMap.prototype.set;
const weakSetAddIntrinsic = WeakSet.prototype.add;
const weakSetHasIntrinsic = WeakSet.prototype.has;

function deleteSetValue<T>(set: Set<T>, value: T): boolean {
  return Reflect.apply(setDeleteIntrinsic, set, [value]) as boolean;
}

function snapshotSetValues<T>(set: Set<T>): readonly T[] {
  const iterator = Reflect.apply(setValuesIntrinsic, set, []) as object;
  const values: T[] = [];
  let index = 0;
  while (true) {
    const step = Reflect.apply(setIteratorNextIntrinsic, iterator, []) as IteratorResult<T>;
    if (step.done) return values;
    values[index] = step.value;
    index += 1;
  }
}

/** Every protocol literal shared by the §4.2–§4.5 message channels. */
export const TSJS_MESSAGE_PROTOCOL_V1 = Object.freeze({
  version: 1 as const,
  rendererVersion: '3' as const,
  message: Object.freeze({
    prebidRequest: 'Prebid Request' as const,
    prebidResponse: 'Prebid Response' as const,
    ownerRegister: 'TS Render Owner Register' as const,
    ownerRegistered: 'TS Render Owner Registered' as const,
    ownerRefused: 'TS Render Owner Refused' as const,
    apsStart: 'TS APS Start' as const,
    admStart: 'TS ADM Start' as const,
    ownerInserted: 'TS Owner Inserted' as const,
    ownerSettled: 'TS Owner Settled' as const,
    admLoaded: 'TS ADM Loaded' as const,
    admFailed: 'TS ADM Failed' as const,
    apsDocumentAccepted: 'TS APS Document Accepted' as const,
    apsRunnerLoaded: 'TS APS Runner Loaded' as const,
    apsRenderCompleted: 'TS APS Render Completed' as const,
    apsRenderFailed: 'TS APS Render Failed' as const,
  }),
  status: Object.freeze({ ready: 'ready' as const, refused: 'refused' as const }),
  kind: Object.freeze({ aps: 'aps' as const, adm: 'adm' as const }),
  outcome: Object.freeze({
    accepted: 'accepted' as const,
    failed: 'failed' as const,
    cancelled: 'cancelled' as const,
  }),
  runnerFailure: Object.freeze({
    descriptorInvalid: 'descriptor_invalid' as const,
    runnerNoLoad: 'runner_no_load' as const,
    runnerFailed: 'runner_failed' as const,
  }),
  cancellation: Object.freeze({
    callerAborted: 'caller_aborted' as const,
    superseded: 'superseded' as const,
    navigationDisposed: 'navigation_disposed' as const,
  }),
});

interface ProtocolMessageSchema {
  readonly transport: 'global-json' | 'structured';
  readonly keys: readonly string[];
  readonly literals: Readonly<Record<string, unknown>>;
}

function schema(
  transport: ProtocolMessageSchema['transport'],
  keys: readonly string[],
  literals: Readonly<Record<string, unknown>>
): ProtocolMessageSchema {
  return Object.freeze({
    transport,
    keys: Object.freeze([...keys]),
    literals: Object.freeze({ ...literals }),
  });
}

/** Exact top-level shapes for every protocol message and nested protocol record. */
export const PROTOCOL_MESSAGE_SCHEMAS_V1 = Object.freeze({
  prebidRequest: schema('global-json', ['message', 'adId', 'adServerDomain'], {
    message: TSJS_MESSAGE_PROTOCOL_V1.message.prebidRequest,
  }),
  ownerRegister: schema('global-json', ['message', 'adId', 'version', 'lifecycleTicket'], {
    message: TSJS_MESSAGE_PROTOCOL_V1.message.ownerRegister,
    version: 1,
  }),
  prebidResponse: schema(
    'structured',
    ['message', 'adId', 'renderer', 'rendererVersion', 'tsOwner'],
    { message: TSJS_MESSAGE_PROTOCOL_V1.message.prebidResponse, rendererVersion: '3' }
  ),
  prebidResponseRefused: schema('structured', ['message', 'adId', 'rendererVersion', 'tsOwner'], {
    message: TSJS_MESSAGE_PROTOCOL_V1.message.prebidResponse,
    rendererVersion: '3',
  }),
  tsOwnerReady: schema('structured', ['version', 'status', 'kind', 'lifecycleTicket'], {
    version: 1,
    status: TSJS_MESSAGE_PROTOCOL_V1.status.ready,
  }),
  tsOwnerRefused: schema('structured', ['version', 'status'], {
    version: 1,
    status: TSJS_MESSAGE_PROTOCOL_V1.status.refused,
  }),
  ownerRegistered: schema('structured', ['message', 'adId', 'version', 'lifecycleTicket'], {
    message: TSJS_MESSAGE_PROTOCOL_V1.message.ownerRegistered,
    version: 1,
  }),
  ownerRefused: schema('structured', ['message', 'adId', 'version'], {
    message: TSJS_MESSAGE_PROTOCOL_V1.message.ownerRefused,
    version: 1,
  }),
  apsStart: schema(
    'structured',
    ['message', 'version', 'lifecycleTicket', 'rendererUrl', 'envelope'],
    { message: TSJS_MESSAGE_PROTOCOL_V1.message.apsStart, version: 1 }
  ),
  apsEnvelope: schema('structured', ['version', 'nonce', 'publisherOrigin', 'renderer'], {
    version: 1,
  }),
  admStart: schema('structured', ['message', 'version', 'lifecycleTicket', 'source'], {
    message: TSJS_MESSAGE_PROTOCOL_V1.message.admStart,
    version: 1,
  }),
  ownerInserted: schema('structured', ['message', 'version', 'lifecycleTicket'], {
    message: TSJS_MESSAGE_PROTOCOL_V1.message.ownerInserted,
    version: 1,
  }),
  admLoaded: schema('structured', ['message', 'version', 'lifecycleTicket'], {
    message: TSJS_MESSAGE_PROTOCOL_V1.message.admLoaded,
    version: 1,
  }),
  admFailed: schema('structured', ['message', 'version', 'lifecycleTicket'], {
    message: TSJS_MESSAGE_PROTOCOL_V1.message.admFailed,
    version: 1,
  }),
  ownerSettledAccepted: schema('structured', ['message', 'version', 'lifecycleTicket', 'outcome'], {
    message: TSJS_MESSAGE_PROTOCOL_V1.message.ownerSettled,
    version: 1,
    outcome: TSJS_MESSAGE_PROTOCOL_V1.outcome.accepted,
  }),
  ownerSettledFailed: schema(
    'structured',
    ['message', 'version', 'lifecycleTicket', 'outcome', 'reason'],
    {
      message: TSJS_MESSAGE_PROTOCOL_V1.message.ownerSettled,
      version: 1,
      outcome: TSJS_MESSAGE_PROTOCOL_V1.outcome.failed,
    }
  ),
  ownerSettledCancelled: schema(
    'structured',
    ['message', 'version', 'lifecycleTicket', 'outcome', 'reason'],
    {
      message: TSJS_MESSAGE_PROTOCOL_V1.message.ownerSettled,
      version: 1,
      outcome: TSJS_MESSAGE_PROTOCOL_V1.outcome.cancelled,
    }
  ),
  apsDocumentAccepted: schema('structured', ['message', 'version', 'nonce'], {
    message: TSJS_MESSAGE_PROTOCOL_V1.message.apsDocumentAccepted,
    version: 1,
  }),
  apsRunnerLoaded: schema('structured', ['message', 'version', 'nonce'], {
    message: TSJS_MESSAGE_PROTOCOL_V1.message.apsRunnerLoaded,
    version: 1,
  }),
  apsRenderCompleted: schema('structured', ['message', 'version', 'nonce'], {
    message: TSJS_MESSAGE_PROTOCOL_V1.message.apsRenderCompleted,
    version: 1,
  }),
  apsRenderFailed: schema('structured', ['message', 'version', 'nonce', 'reason'], {
    message: TSJS_MESSAGE_PROTOCOL_V1.message.apsRenderFailed,
    version: 1,
  }),
});

export type ProtocolMessageKind = keyof typeof PROTOCOL_MESSAGE_SCHEMAS_V1;
export type CaptureMessageListener = (event: MessageEvent) => void;

/** Exact browser event surface owned by the cross-window messaging adapter. */
export interface MessageEventTarget {
  addEventListener(type: 'message', listener: CaptureMessageListener, capture: true): void;
  removeEventListener(type: 'message', listener: CaptureMessageListener, capture: true): void;
  readonly MessageChannel?: new () => { readonly port1: unknown; readonly port2: unknown };
}

/** A narrow owned endpoint for one transferred browser message port. */
export interface MessagingPort {
  post(message: unknown, transferred: readonly unknown[]): boolean;
  listen(
    messageListener: (event: unknown) => void,
    messageErrorListener: (event: unknown) => void
  ): () => void;
  close(): void;
}

/** One locally retained endpoint and one endpoint eligible for exact transfer. */
export interface MessagingChannel {
  readonly retained: MessagingPort;
  readonly transferred: MessagingPort;
}

/** Cross-window boundary consumed by the kernel's capability recognizer. */
export interface MessagingAdapter {
  createChannel(): MessagingChannel | undefined;
  postWindow(
    target: unknown,
    message: unknown,
    targetOrigin: string,
    transferred: readonly MessagingPort[]
  ): boolean;
  installCaptureListener(listener: CaptureMessageListener): () => void;
  parseProtocolMessage(
    kind: ProtocolMessageKind,
    candidate: unknown
  ): Readonly<Record<string, unknown>> | undefined;
  extractTransferredPorts(
    event: unknown,
    expectedCount: 0 | 1 | 2
  ): readonly MessagingPort[] | undefined;
}

/** Semantic validators injected by composition without reversing adapter layering. */
export interface MessagingValidationOptions {
  readonly validateApsRenderer?: (candidate: unknown) => boolean;
  readonly expectedPublisherOrigin?: string;
  readonly expectedRendererUrl?: string;
}

const capabilityPatterns = Object.freeze({
  reservation: /^r1_[A-Za-z0-9_-]{22}$/,
  ticket: /^t1_[A-Za-z0-9_-]{22}$/,
  nonce: /^n1_[A-Za-z0-9_-]{22}$/,
});
const apsRendererKeys = Object.freeze([
  'type',
  'version',
  'accountId',
  'bidId',
  'tagType',
  'creativeUrl',
  'width',
  'height',
  'aaxResponse',
]);
const apsRendererKeysWithCreativeId = Object.freeze([...apsRendererKeys, 'creativeId']);
const encoder = new TextEncoder();
const cancellationReasons = new Set<string>(Object.values(TSJS_MESSAGE_PROTOCOL_V1.cancellation));
const runnerFailureReasons = new Set<string>(Object.values(TSJS_MESSAGE_PROTOCOL_V1.runnerFailure));
const renderFailureReasons = new Set<string>([
  'auction_timeout',
  'auction_disabled',
  'consent_denied',
  'slot_not_eligible',
  'provider_timeout',
  'provider_error',
  'invalid_provider_response',
  'mediation_failed',
  'winner_not_renderable',
  'internal_error',
  'network_error',
  'http_error',
  'invalid_response',
  'slot_unresolved',
  'descriptor_invalid',
  'invalid_dimensions',
  'dimensions_out_of_range',
  'no_render_source',
  'registry_full',
  'capability_registry_full',
  'external_queue_full',
  'external_ready_timeout',
  'external_artifact_incompatible',
  'prebid_admission_failed',
  'prebid_contract_violation',
  'prebid_selection_timeout',
  'reservation_collision',
  'identity_generation_failed',
  'cycle_unattributable',
  'slot_quarantined',
  'gpt_request_failed',
  'gpt_request_timeout',
  'gpt_completion_timeout',
  'reconciliation_capacity',
  'gam_empty',
  'bridge_claim_timeout',
  'bridge_id_mismatch',
  'owner_registration_timeout',
  'owner_insertion_timeout',
  'renderer_document_no_load',
  'runner_no_load',
  'runner_failed',
  'cache_network_error',
  'cache_http_error',
  'cache_invalid_response',
  'adm_document_no_load',
  'abi_mismatch',
  'bundle_partial',
]);

function validUnicodeScalars(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!Number.isInteger(next) || next < 0xdc00 || next > 0xdfff) return false;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return false;
    }
  }
  return true;
}

function boundedString(
  value: unknown,
  maximumBytes: number,
  options: { readonly controls?: boolean; readonly empty?: boolean } = {}
): value is string {
  let hasControl = false;
  if (typeof value === 'string' && options.controls !== true) {
    for (let index = 0; index < value.length; index += 1) {
      const code = value.charCodeAt(index);
      if (code <= 0x1f || code === 0x7f) {
        hasControl = true;
        break;
      }
    }
  }
  return (
    typeof value === 'string' &&
    (options.empty === true || value.length > 0) &&
    validUnicodeScalars(value) &&
    !hasControl &&
    encoder.encode(value).byteLength <= maximumBytes
  );
}

function capability(value: unknown, kind: keyof typeof capabilityPatterns): value is string {
  return typeof value === 'string' && capabilityPatterns[kind].test(value);
}

function dimension(value: unknown): value is number {
  return (
    typeof value === 'number' &&
    Number.isFinite(value) &&
    Number.isInteger(value) &&
    value >= 1 &&
    value <= 4096
  );
}

function exactHttpOrigin(value: unknown): value is string {
  if (!boundedString(value, 2_048)) return false;
  try {
    const parsed = new URL(value);
    return (
      (parsed.protocol === 'http:' || parsed.protocol === 'https:') &&
      parsed.username === '' &&
      parsed.password === '' &&
      parsed.origin === value &&
      parsed.pathname === '/' &&
      parsed.search === '' &&
      parsed.hash === ''
    );
  } catch {
    return false;
  }
}

function rendererUrl(value: unknown, expected?: string): value is string {
  const valid = (candidate: unknown): candidate is string => {
    if (!boundedString(candidate, 2_048)) return false;
    try {
      const parsed = new URL(candidate);
      return (
        (parsed.protocol === 'http:' || parsed.protocol === 'https:') &&
        parsed.hostname !== '' &&
        parsed.username === '' &&
        parsed.password === '' &&
        parsed.pathname === '/integrations/aps/renderer/v1' &&
        parsed.search === '' &&
        parsed.hash === ''
      );
    } catch {
      return false;
    }
  };
  if (!valid(value)) return false;
  if (expected !== undefined) return valid(expected) && value === expected;
  return true;
}

function skipWhitespace(source: string, start: number): number {
  let index = start;
  while (index < source.length && /\s/.test(source[index] ?? '')) index += 1;
  return index;
}

function scanString(source: string, start: number): number | undefined {
  if (source[start] !== '"') return undefined;
  let index = start + 1;
  while (index < source.length) {
    const character = source[index];
    if (character === '"') return index + 1;
    if (character === '\\') {
      index += 1;
      if (index >= source.length) return undefined;
      if (source[index] === 'u') {
        if (!/^[0-9a-fA-F]{4}$/.test(source.slice(index + 1, index + 5))) return undefined;
        index += 4;
      }
    } else if (character !== undefined && character.charCodeAt(0) < 0x20) {
      return undefined;
    }
    index += 1;
  }
  return undefined;
}

function scanJsonValue(source: string, start: number): number | undefined {
  let index = skipWhitespace(source, start);
  if (source[index] === '"') return scanString(source, index);
  if (source[index] === '[') {
    index = skipWhitespace(source, index + 1);
    if (source[index] === ']') return index + 1;
    while (index < source.length) {
      const end = scanJsonValue(source, index);
      if (end === undefined) return undefined;
      index = skipWhitespace(source, end);
      if (source[index] === ']') return index + 1;
      if (source[index] !== ',') return undefined;
      index = skipWhitespace(source, index + 1);
    }
    return undefined;
  }
  if (source[index] === '{') {
    const keys = new Set<string>();
    index = skipWhitespace(source, index + 1);
    if (source[index] === '}') return index + 1;
    while (index < source.length) {
      const keyEnd = scanString(source, index);
      if (keyEnd === undefined) return undefined;
      let key: string;
      try {
        key = JSON.parse(source.slice(index, keyEnd)) as string;
      } catch {
        return undefined;
      }
      if (keys.has(key)) return undefined;
      keys.add(key);
      index = skipWhitespace(source, keyEnd);
      if (source[index] !== ':') return undefined;
      const valueEnd = scanJsonValue(source, index + 1);
      if (valueEnd === undefined) return undefined;
      index = skipWhitespace(source, valueEnd);
      if (source[index] === '}') return index + 1;
      if (source[index] !== ',') return undefined;
      index = skipWhitespace(source, index + 1);
    }
    return undefined;
  }
  const match = /^(?:true|false|null|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)/.exec(
    source.slice(index)
  );
  return match ? index + match[0].length : undefined;
}

function parseGlobalJson(candidate: unknown): unknown {
  if (
    typeof candidate !== 'string' ||
    new TextEncoder().encode(candidate).byteLength > MAX_GLOBAL_MESSAGE_BYTES
  ) {
    return undefined;
  }
  const end = scanJsonValue(candidate, 0);
  if (end === undefined || skipWhitespace(candidate, end) !== candidate.length) return undefined;
  try {
    return JSON.parse(candidate);
  } catch {
    return undefined;
  }
}

function exactRecord(
  candidate: unknown,
  keys: readonly string[]
): Readonly<Record<string, unknown>> | undefined {
  try {
    if (typeof candidate !== 'object' || candidate === null) {
      return undefined;
    }
    const prototype = Object.getPrototypeOf(candidate);
    if (prototype !== Object.prototype && prototype !== null) return undefined;
    const ownKeys = Reflect.ownKeys(candidate);
    const descriptors = Object.getOwnPropertyDescriptors(candidate);
    if (
      ownKeys.length !== keys.length ||
      ownKeys.some((key) => typeof key !== 'string') ||
      keys.some((key) => !ownKeys.includes(key))
    ) {
      return undefined;
    }
    const accepted: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    for (const key of keys) {
      const descriptor = descriptors[key];
      if (!descriptor || !Object.prototype.hasOwnProperty.call(descriptor, 'value')) {
        return undefined;
      }
      accepted[key] = descriptor.value;
    }
    return Object.freeze(accepted);
  } catch {
    return undefined;
  }
}

function admSource(candidate: unknown): boolean {
  const source = exactRecord(candidate, ['type', 'version', 'adm', 'width', 'height']);
  return (
    source !== undefined &&
    source['type'] === 'adm' &&
    source['version'] === 1 &&
    boundedString(source['adm'], 512 * 1024, { controls: true }) &&
    dimension(source['width']) &&
    dimension(source['height'])
  );
}

function canonicalApsRenderer(candidate: unknown): Readonly<Record<string, unknown>> | undefined {
  const renderer =
    exactRecord(candidate, apsRendererKeys) ??
    exactRecord(candidate, apsRendererKeysWithCreativeId);
  if (!renderer) return undefined;
  for (const value of Object.values(renderer)) {
    if (!['string', 'number'].includes(typeof value)) return undefined;
  }
  return renderer;
}

function canonicalApsEnvelope(
  candidate: unknown,
  options: MessagingValidationOptions
): Readonly<Record<string, unknown>> | undefined {
  const envelope = exactRecord(candidate, ['version', 'nonce', 'publisherOrigin', 'renderer']);
  if (
    envelope === undefined ||
    envelope['version'] !== 1 ||
    !capability(envelope['nonce'], 'nonce') ||
    !exactHttpOrigin(envelope['publisherOrigin']) ||
    options.expectedPublisherOrigin === undefined ||
    envelope['publisherOrigin'] !== options.expectedPublisherOrigin
  ) {
    return undefined;
  }
  const renderer = canonicalApsRenderer(envelope['renderer']);
  if (!renderer || options.validateApsRenderer?.(renderer) !== true) return undefined;
  return replaceNested(envelope, ['version', 'nonce', 'publisherOrigin', 'renderer'], { renderer });
}

function parseTsOwner(candidate: unknown): Readonly<Record<string, unknown>> | undefined {
  const ready = exactRecord(candidate, ['version', 'status', 'kind', 'lifecycleTicket']);
  if (ready) {
    return ready['version'] === 1 &&
      ready['status'] === TSJS_MESSAGE_PROTOCOL_V1.status.ready &&
      (ready['kind'] === TSJS_MESSAGE_PROTOCOL_V1.kind.aps ||
        ready['kind'] === TSJS_MESSAGE_PROTOCOL_V1.kind.adm) &&
      capability(ready['lifecycleTicket'], 'ticket')
      ? ready
      : undefined;
  }
  const refused = exactRecord(candidate, ['version', 'status']);
  return refused !== undefined &&
    refused['version'] === 1 &&
    refused['status'] === TSJS_MESSAGE_PROTOCOL_V1.status.refused
    ? refused
    : undefined;
}

function validProtocolFields(
  kind: ProtocolMessageKind,
  record: Readonly<Record<string, unknown>>,
  options: MessagingValidationOptions
): boolean {
  const ticket = (): boolean => capability(record['lifecycleTicket'], 'ticket');
  const nonce = (): boolean => capability(record['nonce'], 'nonce');
  switch (kind) {
    case 'prebidRequest':
      return (
        capability(record['adId'], 'reservation') && boundedString(record['adServerDomain'], 2_048)
      );
    case 'ownerRegister':
      return capability(record['adId'], 'reservation') && ticket();
    case 'prebidResponse': {
      const owner = parseTsOwner(record['tsOwner']);
      return (
        capability(record['adId'], 'reservation') &&
        boundedString(record['renderer'], 64 * 1024, { controls: true }) &&
        owner?.['status'] === TSJS_MESSAGE_PROTOCOL_V1.status.ready
      );
    }
    case 'prebidResponseRefused': {
      const owner = parseTsOwner(record['tsOwner']);
      return (
        capability(record['adId'], 'reservation') &&
        owner?.['status'] === TSJS_MESSAGE_PROTOCOL_V1.status.refused
      );
    }
    case 'tsOwnerReady':
      return (
        (record['kind'] === TSJS_MESSAGE_PROTOCOL_V1.kind.aps ||
          record['kind'] === TSJS_MESSAGE_PROTOCOL_V1.kind.adm) &&
        ticket()
      );
    case 'tsOwnerRefused':
      return true;
    case 'ownerRegistered':
      return capability(record['adId'], 'reservation') && ticket();
    case 'ownerRefused':
      return capability(record['adId'], 'reservation');
    case 'apsStart':
      return (
        ticket() &&
        options.expectedRendererUrl !== undefined &&
        rendererUrl(record['rendererUrl'], options.expectedRendererUrl)
      );
    case 'apsEnvelope':
      return true;
    case 'admStart':
      return ticket() && admSource(record['source']);
    case 'ownerInserted':
    case 'admLoaded':
    case 'admFailed':
      return ticket();
    case 'ownerSettledAccepted':
      return ticket();
    case 'ownerSettledFailed':
      return (
        ticket() &&
        typeof record['reason'] === 'string' &&
        renderFailureReasons.has(record['reason'])
      );
    case 'ownerSettledCancelled':
      return (
        ticket() &&
        typeof record['reason'] === 'string' &&
        cancellationReasons.has(record['reason'])
      );
    case 'apsDocumentAccepted':
    case 'apsRunnerLoaded':
    case 'apsRenderCompleted':
      return nonce();
    case 'apsRenderFailed':
      return (
        nonce() &&
        typeof record['reason'] === 'string' &&
        runnerFailureReasons.has(record['reason'])
      );
  }
}

function replaceNested(
  record: Readonly<Record<string, unknown>>,
  keys: readonly string[],
  replacements: Readonly<Record<string, unknown>>
): Readonly<Record<string, unknown>> {
  const output: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
  for (const key of keys) {
    output[key] = Object.prototype.hasOwnProperty.call(replacements, key)
      ? replacements[key]
      : record[key];
  }
  return Object.freeze(output);
}

function canonicalProtocolRecord(
  kind: ProtocolMessageKind,
  record: Readonly<Record<string, unknown>>,
  keys: readonly string[],
  options: MessagingValidationOptions
): Readonly<Record<string, unknown>> | undefined {
  if (kind === 'prebidResponse' || kind === 'prebidResponseRefused') {
    const owner = parseTsOwner(record['tsOwner']);
    return owner ? replaceNested(record, keys, { tsOwner: owner }) : undefined;
  }
  if (kind === 'apsStart' || kind === 'apsEnvelope') {
    if (
      kind === 'apsStart' &&
      (!capability(record['lifecycleTicket'], 'ticket') ||
        options.expectedRendererUrl === undefined ||
        !rendererUrl(record['rendererUrl'], options.expectedRendererUrl))
    ) {
      return undefined;
    }
    const candidate = kind === 'apsStart' ? record['envelope'] : record;
    const canonicalEnvelope = canonicalApsEnvelope(candidate, options);
    if (!canonicalEnvelope) return undefined;
    return kind === 'apsStart'
      ? replaceNested(record, keys, { envelope: canonicalEnvelope })
      : canonicalEnvelope;
  }
  if (kind === 'admStart') {
    const source = exactRecord(record['source'], ['type', 'version', 'adm', 'width', 'height']);
    return source ? replaceNested(record, keys, { source }) : undefined;
  }
  return record;
}

function parseProtocolMessage(
  kind: ProtocolMessageKind,
  candidate: unknown,
  options: MessagingValidationOptions
): Readonly<Record<string, unknown>> | undefined {
  try {
    const messageSchema = (
      PROTOCOL_MESSAGE_SCHEMAS_V1 as Readonly<Record<string, ProtocolMessageSchema>>
    )[kind];
    if (!messageSchema) return undefined;
    const decoded =
      messageSchema.transport === 'global-json' ? parseGlobalJson(candidate) : candidate;
    const accepted = exactRecord(decoded, messageSchema.keys);
    if (!accepted) return undefined;
    for (const [key, literal] of Object.entries(messageSchema.literals)) {
      if (accepted[key] !== literal) return undefined;
    }
    const canonical = canonicalProtocolRecord(kind, accepted, messageSchema.keys, options);
    if (!canonical) return undefined;
    if (!validProtocolFields(kind, canonical, options)) return undefined;
    if (
      kind === 'prebidResponse' &&
      encoder.encode(JSON.stringify(canonical)).byteLength > 72 * 1024
    ) {
      return undefined;
    }
    return canonical;
  } catch {
    return undefined;
  }
}

interface CapturedPortClose {
  readonly binding: object;
  readonly closePort: (...arguments_: unknown[]) => unknown;
}

interface RawPort extends CapturedPortClose {
  readonly add: (...arguments_: unknown[]) => unknown;
  readonly postMessage: (...arguments_: unknown[]) => unknown;
  readonly remove: (...arguments_: unknown[]) => unknown;
  readonly start?: (...arguments_: unknown[]) => unknown;
}

interface RawPortInspection {
  readonly close: CapturedPortClose | undefined;
  readonly raw: RawPort | undefined;
}

interface WrappedPortState {
  readonly raw: RawPort;
  readonly transferable: boolean;
  closed: boolean;
  transferred: boolean;
  transferring: boolean;
}

interface TransferReservation {
  readonly rawTransfers: readonly object[];
  readonly states: readonly WrappedPortState[];
}

const wrappedPortStates = new WeakMap<MessagingPort, WrappedPortState>();
const ownedPortBindings = new WeakSet<object>();

function getWrappedPortState(port: MessagingPort): WrappedPortState | undefined {
  return Reflect.apply(weakMapGetIntrinsic, wrappedPortStates, [port]) as
    WrappedPortState | undefined;
}

function setWrappedPortState(port: MessagingPort, state: WrappedPortState): void {
  Reflect.apply(weakMapSetIntrinsic, wrappedPortStates, [port, state]);
}

function ownsPortBinding(binding: object): boolean {
  return Reflect.apply(weakSetHasIntrinsic, ownedPortBindings, [binding]) as boolean;
}

function claimPortBinding(binding: object): void {
  Reflect.apply(weakSetAddIntrinsic, ownedPortBindings, [binding]);
}

function portCandidateBinding(candidate: unknown): object | undefined {
  return (typeof candidate === 'object' && candidate !== null) || typeof candidate === 'function'
    ? (candidate as object)
    : undefined;
}

function claimPortCandidate(candidate: unknown): boolean {
  const binding = portCandidateBinding(candidate);
  if (!binding || ownsPortBinding(binding)) return false;
  claimPortBinding(binding);
  return true;
}

function inspectRawPort(candidate: unknown): RawPortInspection {
  const binding = portCandidateBinding(candidate);
  if (!binding) return { close: undefined, raw: undefined };
  let closePort: unknown;
  try {
    closePort = Reflect.get(binding, 'close');
  } catch {
    return { close: undefined, raw: undefined };
  }
  if (typeof closePort !== 'function') return { close: undefined, raw: undefined };
  const callableClose = closePort as (...arguments_: unknown[]) => unknown;
  const close: CapturedPortClose = { binding, closePort: callableClose };
  try {
    const add = Reflect.get(binding, 'addEventListener');
    const postMessage = Reflect.get(binding, 'postMessage');
    const remove = Reflect.get(binding, 'removeEventListener');
    const start = Reflect.get(binding, 'start');
    if (
      typeof add !== 'function' ||
      typeof postMessage !== 'function' ||
      typeof remove !== 'function' ||
      (start !== undefined && typeof start !== 'function')
    ) {
      return { close, raw: undefined };
    }
    return {
      close,
      raw: { binding, add, closePort: callableClose, postMessage, remove, start },
    };
  } catch {
    return { close, raw: undefined };
  }
}

function closeRawPort(candidate: unknown): void {
  try {
    if ((typeof candidate !== 'object' || candidate === null) && typeof candidate !== 'function') {
      return;
    }
    const close = Reflect.get(candidate, 'close');
    if (typeof close === 'function') Reflect.apply(close, candidate, []);
  } catch {
    // Closing one invalid port cannot interrupt cleanup of the remaining ports.
  }
}

function closeCapturedRawPort(raw: CapturedPortClose): void {
  try {
    Reflect.apply(raw.closePort, raw.binding, []);
  } catch {
    // A captured endpoint close cannot interrupt channel-construction cleanup.
  }
}

function wrapPort(raw: RawPort, transferable = false): MessagingPort {
  const listeners = new Set<() => void>();
  const state: WrappedPortState = {
    raw,
    transferable,
    closed: false,
    transferred: false,
    transferring: false,
  };
  const port: MessagingPort = Object.freeze({
    post: (message: unknown, transferred: readonly unknown[]): boolean => {
      if (state.transferable || state.closed || state.transferred || state.transferring) {
        return false;
      }
      const reservation = reserveTransferPorts(transferred);
      if (!reservation) return false;
      try {
        Reflect.apply(raw.postMessage, raw.binding, [message, reservation.rawTransfers]);
      } catch {
        rollbackTransferReservation(reservation);
        return false;
      }
      commitTransferReservation(reservation);
      return true;
    },
    listen: (
      messageListener: (event: unknown) => void,
      messageErrorListener: (event: unknown) => void
    ): (() => void) => {
      if (state.transferable || state.closed || state.transferred || state.transferring) {
        return () => undefined;
      }
      const wrappedMessage = (event: unknown): void => {
        if (state.closed || state.transferred) return;
        try {
          messageListener(event);
        } catch {
          // Channel callbacks cannot escape the messaging boundary.
        }
      };
      const wrappedMessageError = (event: unknown): void => {
        if (state.closed || state.transferred) return;
        try {
          messageErrorListener(event);
        } catch {
          // Message deserialization failures remain contained by the channel boundary.
        }
      };
      let messageAttempted = false;
      let messageErrorAttempted = false;
      let setupInProgress = true;
      const rollback = (): void => {
        if (messageErrorAttempted) {
          messageErrorAttempted = false;
          try {
            Reflect.apply(raw.remove, raw.binding, ['messageerror', wrappedMessageError]);
          } catch {
            // One listener cleanup cannot interrupt rollback of the other listener.
          }
        }
        if (messageAttempted) {
          messageAttempted = false;
          try {
            Reflect.apply(raw.remove, raw.binding, ['message', wrappedMessage]);
          } catch {
            // Listener cleanup remains best-effort during terminal port disposal.
          }
        }
      };
      let active = true;
      const dispose = (): void => {
        if (!active) return;
        active = false;
        try {
          deleteSetValue(listeners, dispose);
        } finally {
          if (!setupInProgress) rollback();
        }
      };
      const stopClosedSetup = (): boolean => {
        if (!state.closed && !state.transferred && !state.transferring && active) return false;
        setupInProgress = false;
        rollback();
        return true;
      };
      try {
        listeners.add(dispose);
      } catch {
        setupInProgress = false;
        active = false;
        try {
          deleteSetValue(listeners, dispose);
        } catch {
          // Failed bookkeeping cannot retain listener ownership.
        } finally {
          rollback();
        }
        return dispose;
      }
      try {
        messageAttempted = true;
        Reflect.apply(raw.add, raw.binding, ['message', wrappedMessage]);
        if (stopClosedSetup()) return dispose;
        messageErrorAttempted = true;
        Reflect.apply(raw.add, raw.binding, ['messageerror', wrappedMessageError]);
        if (stopClosedSetup()) return dispose;
        if (raw.start) {
          Reflect.apply(raw.start, raw.binding, []);
          if (stopClosedSetup()) return dispose;
        }
      } catch {
        setupInProgress = false;
        active = false;
        try {
          deleteSetValue(listeners, dispose);
        } catch {
          // Failed bookkeeping cannot interrupt exact listener rollback.
        } finally {
          rollback();
        }
        return dispose;
      }
      setupInProgress = false;
      return dispose;
    },
    close: (): void => {
      if (state.closed || state.transferred || state.transferring) return;
      state.closed = true;
      let disposers: readonly (() => void)[] = [];
      try {
        disposers = snapshotSetValues(listeners);
      } catch {
        // The captured native iterator should be total for the private native Set.
      }
      for (let index = 0; index < disposers.length; index += 1) {
        try {
          disposers[index]?.();
        } catch {
          // One listener cleanup cannot skip the remaining listeners or raw close.
        }
      }
      try {
        Reflect.apply(raw.closePort, raw.binding, []);
      } catch {
        // Closing remains best-effort and idempotent.
      }
    },
  });
  setWrappedPortState(port, state);
  return port;
}

function snapshotPortArray(
  candidate: unknown
): { readonly valid: boolean; readonly values: readonly unknown[] } | undefined {
  try {
    if (!Array.isArray(candidate) || Object.getPrototypeOf(candidate) !== Array.prototype) {
      return undefined;
    }
    const lengthDescriptor = Object.getOwnPropertyDescriptor(candidate, 'length');
    if (
      !lengthDescriptor ||
      !Object.prototype.hasOwnProperty.call(lengthDescriptor, 'value') ||
      typeof lengthDescriptor.value !== 'number' ||
      !Number.isSafeInteger(lengthDescriptor.value) ||
      lengthDescriptor.value < 0
    ) {
      return undefined;
    }
    const length = lengthDescriptor.value;
    const ownKeys = Reflect.ownKeys(candidate);
    const values: unknown[] = [];
    let valid = length <= 2 && ownKeys.length === length + 1;
    if (length <= 2) {
      for (let keyIndex = 0; keyIndex < ownKeys.length; keyIndex += 1) {
        const key = ownKeys[keyIndex];
        if (key === 'length') continue;
        let expected = false;
        for (let valueIndex = 0; valueIndex < length; valueIndex += 1) {
          if (key === String(valueIndex)) {
            expected = true;
            break;
          }
        }
        if (!expected) valid = false;
      }
      for (let index = 0; index < length; index += 1) {
        const descriptor = Object.getOwnPropertyDescriptor(candidate, String(index));
        if (!descriptor || !Object.prototype.hasOwnProperty.call(descriptor, 'value')) {
          valid = false;
          continue;
        }
        values[values.length] = descriptor.value;
      }
    } else {
      for (let keyIndex = 0; keyIndex < ownKeys.length; keyIndex += 1) {
        const key = ownKeys[keyIndex];
        if (typeof key !== 'string' || key === 'length') continue;
        const index = Number(key);
        if (!Number.isSafeInteger(index) || index < 0 || index >= length || String(index) !== key) {
          continue;
        }
        const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
        if (descriptor && Object.prototype.hasOwnProperty.call(descriptor, 'value')) {
          values[values.length] = descriptor.value;
        }
      }
    }
    return { valid, values };
  } catch {
    return undefined;
  }
}

function reserveTransferPorts(transferred: readonly unknown[]): TransferReservation | undefined {
  const snapshot = snapshotPortArray(transferred);
  if (!snapshot?.valid) return undefined;
  const states: WrappedPortState[] = [];
  const rawTransfers: object[] = [];
  for (let index = 0; index < snapshot.values.length; index += 1) {
    const port = snapshot.values[index];
    const state = getWrappedPortState(port as MessagingPort);
    if (!state || !state.transferable || state.closed || state.transferred || state.transferring) {
      return undefined;
    }
    for (let prior = 0; prior < states.length; prior += 1) {
      if (states[prior] === state) return undefined;
    }
    states[index] = state;
    rawTransfers[index] = state.raw.binding;
  }
  for (let index = 0; index < states.length; index += 1) {
    const state = states[index];
    if (state) state.transferring = true;
  }
  return { rawTransfers, states };
}

function rollbackTransferReservation(reservation: TransferReservation): void {
  for (let index = 0; index < reservation.states.length; index += 1) {
    const state = reservation.states[index];
    if (state) state.transferring = false;
  }
}

function commitTransferReservation(reservation: TransferReservation): void {
  for (let index = 0; index < reservation.states.length; index += 1) {
    const state = reservation.states[index];
    if (!state) continue;
    state.transferring = false;
    state.transferred = true;
  }
}

function extractTransferredPorts(
  event: unknown,
  expectedCount: 0 | 1 | 2
): readonly MessagingPort[] | undefined {
  let candidates: unknown;
  try {
    if (typeof event !== 'object' || event === null) return undefined;
    candidates = Reflect.get(event, 'ports');
  } catch {
    return undefined;
  }
  const snapshot = snapshotPortArray(candidates);
  if (!snapshot) return undefined;
  const inspections: Array<RawPortInspection | undefined> = [];
  const claimed: boolean[] = [];
  let accepted = snapshot.valid && snapshot.values.length === expectedCount;
  for (let index = 0; index < snapshot.values.length; index += 1) {
    const candidate = snapshot.values[index];
    const candidateClaimed = claimPortCandidate(candidate);
    claimed[index] = candidateClaimed;
    if (!candidateClaimed) {
      accepted = false;
      continue;
    }
    const inspection = inspectRawPort(candidate);
    inspections[index] = inspection;
    if (!inspection.raw) accepted = false;
  }
  if (!accepted) {
    for (let index = 0; index < snapshot.values.length; index += 1) {
      if (!claimed[index]) continue;
      const captured = inspections[index]?.close;
      if (captured) closeCapturedRawPort(captured);
      else closeRawPort(snapshot.values[index]);
    }
    return undefined;
  }
  const wrapped: MessagingPort[] = [];
  try {
    for (let index = 0; index < inspections.length; index += 1) {
      const raw = inspections[index]?.raw;
      if (!raw) throw new Error('Accepted raw port inspection is unavailable');
      wrapped[index] = wrapPort(raw);
    }
    return Object.freeze(wrapped);
  } catch {
    for (let index = 0; index < inspections.length; index += 1) {
      const captured = inspections[index]?.close;
      if (claimed[index] && captured) closeCapturedRawPort(captured);
    }
    return undefined;
  }
}

function createChannel(target: MessageEventTarget): MessagingChannel | undefined {
  let first: unknown;
  let second: unknown;
  let retainedInspection: RawPortInspection | undefined;
  let transferredInspection: RawPortInspection | undefined;
  let claimedRetained = false;
  let claimedTransferred = false;
  const cleanup = (): void => {
    if (claimedRetained) {
      const captured = retainedInspection?.close;
      if (captured) closeCapturedRawPort(captured);
      else closeRawPort(first);
    }
    if (claimedTransferred) {
      const captured = transferredInspection?.close;
      if (captured) closeCapturedRawPort(captured);
      else closeRawPort(second);
    }
  };
  try {
    const constructor = Reflect.get(target, 'MessageChannel');
    if (typeof constructor !== 'function') return undefined;
    const channel = Reflect.construct(constructor, [] as never[]) as object;
    first = Reflect.get(channel, 'port1');
    claimedRetained = claimPortCandidate(first);
    if (claimedRetained) retainedInspection = inspectRawPort(first);
    second = Reflect.get(channel, 'port2');
    if (second !== first) claimedTransferred = claimPortCandidate(second);
    if (claimedTransferred) transferredInspection = inspectRawPort(second);
    if (first === second) {
      if (claimedRetained) {
        cleanup();
      }
      return undefined;
    }
    const retainedRaw = retainedInspection?.raw;
    const transferredRaw = transferredInspection?.raw;
    if (!claimedRetained || !claimedTransferred || !retainedRaw || !transferredRaw) {
      cleanup();
      return undefined;
    }
    return Object.freeze({
      retained: wrapPort(retainedRaw),
      transferred: wrapPort(transferredRaw, true),
    });
  } catch {
    cleanup();
    return undefined;
  }
}

function postWindow(
  target: unknown,
  message: unknown,
  targetOrigin: string,
  transferred: readonly MessagingPort[]
): boolean {
  let postMessage: unknown;
  try {
    if (
      ((typeof target !== 'object' || target === null) && typeof target !== 'function') ||
      typeof targetOrigin !== 'string' ||
      targetOrigin.length === 0 ||
      targetOrigin.length > 2_048
    ) {
      return false;
    }
    postMessage = Reflect.get(target, 'postMessage');
    if (typeof postMessage !== 'function') return false;
  } catch {
    return false;
  }
  const reservation = reserveTransferPorts(transferred);
  if (!reservation) return false;
  try {
    Reflect.apply(postMessage, target, [message, targetOrigin, reservation.rawTransfers]);
  } catch {
    rollbackTransferReservation(reservation);
    return false;
  }
  commitTransferReservation(reservation);
  return true;
}

/**
 * Create the production messaging boundary.
 *
 * Listener installation is deliberately synchronous so core can reserve a
 * capability message before any integration activation or TS-owned injection.
 */
export function createBrowserMessagingAdapter(
  target: MessageEventTarget = window as unknown as MessageEventTarget,
  validation: MessagingValidationOptions = {}
): MessagingAdapter {
  return Object.freeze({
    createChannel: () => createChannel(target),
    postWindow,
    installCaptureListener(listener: CaptureMessageListener): () => void {
      let add: unknown;
      let remove: unknown;
      try {
        add = Reflect.get(target, 'addEventListener');
        remove = Reflect.get(target, 'removeEventListener');
      } catch {
        return () => undefined;
      }
      if (typeof add !== 'function' || typeof remove !== 'function') return () => undefined;
      const wrapped: CaptureMessageListener = (event): void => {
        try {
          listener(event);
        } catch {
          // Capture listener failures cannot escape the global dispatcher boundary.
        }
      };
      let attempted = false;
      const rollback = (): void => {
        if (!attempted) return;
        attempted = false;
        try {
          Reflect.apply(remove, target, ['message', wrapped, true]);
        } catch {
          // Capture listener cleanup remains best-effort.
        }
      };
      try {
        attempted = true;
        Reflect.apply(add, target, ['message', wrapped, true]);
      } catch {
        rollback();
        return () => undefined;
      }
      return () => {
        rollback();
      };
    },
    parseProtocolMessage: (kind: ProtocolMessageKind, candidate: unknown) =>
      parseProtocolMessage(kind, candidate, validation),
    extractTransferredPorts,
  });
}

/** Create a side-effect-free messaging boundary for tests and non-DOM runtimes. */
export function createNoopMessagingAdapter(): MessagingAdapter {
  return Object.freeze({
    createChannel: () => undefined,
    postWindow: () => false,
    installCaptureListener: () => () => undefined,
    parseProtocolMessage: (kind: ProtocolMessageKind, candidate: unknown) =>
      parseProtocolMessage(kind, candidate, {}),
    extractTransferredPorts,
  });
}
