import type { MessagingAdapter, MessagingPort } from '../adapters/messaging';
import { TSJS_MESSAGE_PROTOCOL_V1 } from '../kernel/contracts/message_protocol';
import type { IdentityGenerationResult } from '../kernel/identity';

interface CollapsedPucShellResizeInput {
  readonly source: object;
  readonly width: number;
  readonly height: number;
}

import type {
  CommittedRenderArtifact,
  RenderAttempt,
  RenderFailureReason,
  RenderOutcome,
  RendererNonceRegistry,
} from './render';
import type {
  ReservationAttempt,
  ReservationRecognition,
  ReservationRenderSource,
  ReservationService,
} from './reservations';

const CLAIM_DEADLINE_MS = 3_000;
const LIFECYCLE_TICKET_TTL_MS = 3_000;
const MAX_DYNAMIC_OWNER_BYTES = 64 * 1_024;
const MAX_OUTER_RESPONSE_BYTES = 72 * 1_024;
const MAX_TICKET_DRAWS = 8;
const MAX_TICKETS = 320;
const RESERVATION_ID = /^r1_[A-Za-z0-9_-]{22}$/;
const ATTEMPT_ID = /^a1_[A-Za-z0-9_-]{22}$/;
const LIFECYCLE_TICKET = /^t1_[A-Za-z0-9_-]{22}$/;
const textEncoder = new TextEncoder();
const textEncoderEncodeIntrinsic = TextEncoder.prototype.encode;
const mapGetIntrinsic = Map.prototype.get;
const mapSetIntrinsic = Map.prototype.set;
const mapDeleteIntrinsic = Map.prototype.delete;
const mapClearIntrinsic = Map.prototype.clear;
const mapEntriesIntrinsic = Map.prototype.entries;
const mapSizeGetter = Object.getOwnPropertyDescriptor(Map.prototype, 'size')?.get as (
  this: Map<unknown, unknown>
) => number;
const mapValuesIntrinsic = Map.prototype.values;
const mapEntryIteratorNextIntrinsic = Object.getPrototypeOf(new Map().entries()).next as (
  this: IterableIterator<unknown>
) => IteratorResult<unknown>;
const mapIteratorNextIntrinsic = Object.getPrototypeOf(new Map().values()).next as (
  this: IterableIterator<unknown>
) => IteratorResult<unknown>;
const jsonStringifyIntrinsic = JSON.stringify;
const objectFreezeIntrinsic = Object.freeze;

/**
 * Install the self-contained renderer that PUC evaluates in its hidden frame.
 *
 * This function deliberately closes over nothing: its serialized source is the
 * exact program returned in the successful outer PUC response.
 */
function installPucDynamicOwner(): void {
  const ownerWindow = window as Window & {
    render?: (data: unknown, helper: unknown, creativeWindow: Window) => Promise<void>;
  };
  const ticketPattern = /^t1_[A-Za-z0-9_-]{22}$/;
  const reservationPattern = /^r1_[A-Za-z0-9_-]{22}$/;
  const admSandbox =
    'allow-forms allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation';
  const apsSandbox =
    'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation';
  const renderFailureReasons = new Set([
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
    'adm_document_no_load',
    'abi_mismatch',
    'bundle_partial',
  ]);
  const cancellationReasons = new Set(['caller_aborted', 'superseded', 'navigation_disposed']);
  const messageEventDataGetter = Object.getOwnPropertyDescriptor(MessageEvent.prototype, 'data')
    ?.get as ((this: MessageEvent) => unknown) | undefined;
  const messageEventPortsGetter = Object.getOwnPropertyDescriptor(MessageEvent.prototype, 'ports')
    ?.get as ((this: MessageEvent) => readonly MessagePort[]) | undefined;

  const ownDataValue = (candidate: unknown, name: string): unknown => {
    try {
      if (typeof candidate !== 'object' || candidate === null) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(candidate, name);
      return descriptor && 'value' in descriptor ? descriptor.value : undefined;
    } catch {
      return undefined;
    }
  };
  const eventDataValue = (event: unknown): unknown => {
    try {
      if (typeof event !== 'object' || event === null) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(event, 'data');
      if (descriptor) return 'value' in descriptor ? descriptor.value : undefined;
      return messageEventDataGetter ? Reflect.apply(messageEventDataGetter, event, []) : undefined;
    } catch {
      return undefined;
    }
  };

  const exactRecord = (
    candidate: unknown,
    keys: readonly string[]
  ): Record<string, unknown> | undefined => {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      Object.getOwnPropertySymbols(candidate).length !== 0
    ) {
      return undefined;
    }
    const names = Object.getOwnPropertyNames(candidate).sort();
    const expected = [...keys].sort();
    if (names.length !== expected.length) return undefined;
    for (let index = 0; index < expected.length; index += 1) {
      if (names[index] !== expected[index]) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(candidate, expected[index] as string);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
    }
    return candidate as Record<string, unknown>;
  };
  const inspectEventPorts = (
    event: unknown
  ):
    | Readonly<{
        exactShape: boolean;
        originalCount: number;
        ports: readonly MessagePort[];
      }>
    | undefined => {
    try {
      if (typeof event !== 'object' || event === null) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(event, 'ports');
      const ports = descriptor
        ? 'value' in descriptor
          ? descriptor.value
          : undefined
        : messageEventPortsGetter
          ? Reflect.apply(messageEventPortsGetter, event, [])
          : undefined;
      if (!Array.isArray(ports)) return undefined;
      const length = Object.getOwnPropertyDescriptor(ports, 'length');
      if (
        !length ||
        !('value' in length) ||
        !Number.isSafeInteger(length.value) ||
        length.value < 0
      ) {
        return undefined;
      }
      let exactShape =
        Object.getPrototypeOf(ports) === Array.prototype &&
        Object.getOwnPropertySymbols(ports).length === 0 &&
        Object.getOwnPropertyNames(ports).length === length.value + 1;
      const snapshot: MessagePort[] = [];
      const seen = new Set<MessagePort>();
      for (let index = 0; index < length.value; index += 1) {
        const descriptor = Object.getOwnPropertyDescriptor(ports, String(index));
        if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) {
          exactShape = false;
          continue;
        }
        const port = descriptor.value as Partial<MessagePort> | undefined;
        let validPort = false;
        try {
          validPort =
            !!port &&
            typeof Reflect.get(port, 'postMessage') === 'function' &&
            typeof Reflect.get(port, 'close') === 'function';
        } catch {
          validPort = false;
        }
        if (!port || !validPort) {
          exactShape = false;
          continue;
        }
        const accepted = port as MessagePort;
        if (seen.has(accepted)) {
          exactShape = false;
          continue;
        }
        seen.add(accepted);
        snapshot[snapshot.length] = accepted;
      }
      return { exactShape, originalCount: length.value, ports: snapshot };
    } catch {
      return undefined;
    }
  };
  const eventPorts = (event: unknown, count: number): MessagePort[] | undefined => {
    const inspection = inspectEventPorts(event);
    return inspection?.exactShape === true &&
      inspection.originalCount === count &&
      inspection.ports.length === count
      ? [...inspection.ports]
      : undefined;
  };
  const closeEventPorts = (event: unknown): void => {
    const inspection = inspectEventPorts(event);
    if (!inspection) return;
    for (let index = 0; index < inspection.ports.length; index += 1) {
      try {
        inspection.ports[index]?.close();
      } catch {
        // Late or malformed endpoints are still contained independently.
      }
    }
  };
  const skipJsonWhitespace = (source: string, start: number): number => {
    let index = start;
    while (
      source[index] === ' ' ||
      source[index] === '\t' ||
      source[index] === '\n' ||
      source[index] === '\r'
    ) {
      index += 1;
    }
    return index;
  };
  const scanJsonString = (source: string, start: number): number | undefined => {
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
  };
  const scanJsonValue = (source: string, start: number): number | undefined => {
    let index = skipJsonWhitespace(source, start);
    if (source[index] === '"') return scanJsonString(source, index);
    if (source[index] === '[') {
      index = skipJsonWhitespace(source, index + 1);
      if (source[index] === ']') return index + 1;
      while (index < source.length) {
        const end = scanJsonValue(source, index);
        if (end === undefined) return undefined;
        index = skipJsonWhitespace(source, end);
        if (source[index] === ']') return index + 1;
        if (source[index] !== ',') return undefined;
        index = skipJsonWhitespace(source, index + 1);
      }
      return undefined;
    }
    if (source[index] === '{') {
      const keys = new Set<string>();
      index = skipJsonWhitespace(source, index + 1);
      if (source[index] === '}') return index + 1;
      while (index < source.length) {
        const keyEnd = scanJsonString(source, index);
        if (keyEnd === undefined) return undefined;
        let key: unknown;
        try {
          key = JSON.parse(source.slice(index, keyEnd)) as unknown;
        } catch {
          return undefined;
        }
        if (typeof key !== 'string' || keys.has(key)) return undefined;
        keys.add(key);
        index = skipJsonWhitespace(source, keyEnd);
        if (source[index] !== ':') return undefined;
        const valueEnd = scanJsonValue(source, index + 1);
        if (valueEnd === undefined) return undefined;
        index = skipJsonWhitespace(source, valueEnd);
        if (source[index] === '}') return index + 1;
        if (source[index] !== ',') return undefined;
        index = skipJsonWhitespace(source, index + 1);
      }
      return undefined;
    }
    const match = /^(?:true|false|null|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)/.exec(
      source.slice(index)
    );
    return match ? index + match[0].length : undefined;
  };
  const parseJsonWithoutDuplicateKeys = (source: string): unknown => {
    const end = scanJsonValue(source, 0);
    if (end === undefined || skipJsonWhitespace(source, end) !== source.length) return undefined;
    try {
      return JSON.parse(source) as unknown;
    } catch {
      return undefined;
    }
  };
  const parseRegistration = (value: unknown): Record<string, unknown> | undefined => {
    try {
      if (typeof value !== 'string' || new TextEncoder().encode(value).byteLength > 4096) {
        return undefined;
      }
      return exactRecord(parseJsonWithoutDuplicateKeys(value), [
        'message',
        'adId',
        'version',
        'lifecycleTicket',
      ]);
    } catch {
      return undefined;
    }
  };
  const validDimension = (value: unknown): value is number =>
    typeof value === 'number' && Number.isInteger(value) && value >= 1 && value <= 4096;
  const validAdmSource = (value: unknown): value is Record<string, unknown> => {
    const source = exactRecord(value, ['type', 'version', 'adm', 'width', 'height']);
    if (
      !source ||
      source['type'] !== 'adm' ||
      source['version'] !== 1 ||
      typeof source['adm'] !== 'string' ||
      source['adm'].trim().length === 0 ||
      new TextEncoder().encode(source['adm']).byteLength > 512 * 1024 ||
      !validDimension(source['width']) ||
      !validDimension(source['height'])
    ) {
      return false;
    }
    return true;
  };
  const utf8Length = (value: string): number => new TextEncoder().encode(value).byteLength;
  const containsAsciiControl = (value: string): boolean =>
    Array.from(value).some((character) => {
      const codePoint = character.codePointAt(0);
      return codePoint !== undefined && (codePoint <= 0x1f || codePoint === 0x7f);
    });
  const validApsRenderer = (renderer: Record<string, unknown>, publisherOrigin: URL): boolean => {
    const accountId = renderer['accountId'];
    const bidId = renderer['bidId'];
    const creativeId = renderer['creativeId'];
    const creativeUrl = renderer['creativeUrl'];
    const aaxResponse = renderer['aaxResponse'];
    if (
      renderer['type'] !== 'aps' ||
      renderer['version'] !== 1 ||
      typeof accountId !== 'string' ||
      accountId.length === 0 ||
      utf8Length(accountId) > 1024 ||
      typeof bidId !== 'string' ||
      bidId.length === 0 ||
      utf8Length(bidId) > 64 ||
      containsAsciiControl(bidId) ||
      (renderer['tagType'] !== 'iframe' && renderer['tagType'] !== 'script') ||
      !validDimension(renderer['width']) ||
      !validDimension(renderer['height']) ||
      typeof creativeUrl !== 'string' ||
      utf8Length(creativeUrl) > 4096 ||
      typeof aaxResponse !== 'string' ||
      aaxResponse.length > 349_528 ||
      (Object.prototype.hasOwnProperty.call(renderer, 'creativeId') &&
        (typeof creativeId !== 'string' ||
          creativeId.length === 0 ||
          utf8Length(creativeId) > 1024))
    ) {
      return false;
    }
    try {
      const parsedCreativeUrl = new URL(creativeUrl);
      return (
        parsedCreativeUrl.protocol === 'https:' &&
        parsedCreativeUrl.hostname !== '' &&
        parsedCreativeUrl.username === '' &&
        parsedCreativeUrl.password === '' &&
        parsedCreativeUrl.origin !== publisherOrigin.origin
      );
    } catch {
      return false;
    }
  };

  ownerWindow.render = (data, helper, creativeWindow) =>
    new Promise<void>((resolve, reject) => {
      let outer: Record<string, unknown> | undefined;
      let owner: Record<string, unknown> | undefined;
      let sendMessage: unknown;
      try {
        outer = exactRecord(data, ['adId', 'message', 'renderer', 'rendererVersion', 'tsOwner']);
        owner = outer
          ? exactRecord(outer['tsOwner'], ['version', 'status', 'kind', 'lifecycleTicket'])
          : undefined;
        sendMessage =
          typeof helper === 'object' && helper !== null
            ? Reflect.get(helper, 'sendMessage')
            : undefined;
      } catch {
        outer = undefined;
      }
      const adId = outer?.['adId'];
      const lifecycleTicket = owner?.['lifecycleTicket'];
      const ownerKind = owner?.['kind'];
      if (
        !outer ||
        !owner ||
        outer['message'] !== 'Prebid Response' ||
        outer['rendererVersion'] !== '3' ||
        typeof adId !== 'string' ||
        !reservationPattern.test(adId) ||
        owner['version'] !== 1 ||
        owner['status'] !== 'ready' ||
        (owner['kind'] !== 'aps' && owner['kind'] !== 'adm') ||
        typeof lifecycleTicket !== 'string' ||
        !ticketPattern.test(lifecycleTicket) ||
        typeof sendMessage !== 'function' ||
        !creativeWindow ||
        !creativeWindow.document
      ) {
        reject(new Error('TS render owner input refused'));
        return;
      }

      let settled = false;
      let registrationFinished = false;
      let helperDisposer: (() => void) | undefined;
      let ownerTimer: number | undefined;
      let controlPort: MessagePort | undefined;
      let documentPort: MessagePort | undefined;
      let frame: HTMLIFrameElement | undefined;
      let ownerFrameCurrent: (() => boolean) | undefined;
      let frameCommitted = false;
      let localApsFailure = false;
      let started = false;

      const removeFrameHandlers = (): void => {
        if (!frame) return;
        try {
          frame.onload = null;
        } catch {
          // One hostile DOM setter cannot skip the remaining terminal cleanup.
        }
        try {
          frame.onerror = null;
        } catch {
          // One hostile DOM setter cannot skip the remaining terminal cleanup.
        }
      };
      const clearTimer = (handle: number | undefined): void => {
        if (handle === undefined) return;
        try {
          creativeWindow.clearTimeout(handle);
        } catch {
          // Timer cleanup cannot prevent channel cleanup or Promise settlement.
        }
      };
      const removeFrame = (candidate: HTMLIFrameElement | undefined): void => {
        try {
          candidate?.remove();
        } catch {
          // DOM cleanup is best-effort after authority is already terminal.
        }
      };
      const closePort = (port: MessagePort | undefined): void => {
        try {
          port?.close();
        } catch {
          // Endpoint cleanup remains best-effort after the owner is inert.
        }
      };
      const stopHelper = (): void => {
        const dispose = helperDisposer;
        helperDisposer = undefined;
        try {
          dispose?.();
        } catch {
          // PUC helper cleanup cannot replay owner settlement.
        }
      };
      const finish = (accepted: boolean, reason: string): void => {
        if (settled) return;
        settled = true;
        try {
          clearTimer(registrationTimer);
          clearTimer(ownerTimer);
          stopHelper();
          removeFrameHandlers();
          if (!accepted && frame && !frameCommitted) removeFrame(frame);
          if (controlPort) {
            try {
              controlPort.onmessage = null;
            } catch {
              // One hostile handler setter cannot retain the remaining authority.
            }
            try {
              controlPort.onmessageerror = null;
            } catch {
              // One hostile handler setter cannot retain the remaining authority.
            }
          }
          closePort(documentPort);
          closePort(controlPort);
          documentPort = undefined;
          controlPort = undefined;
          ownerFrameCurrent = undefined;
        } finally {
          if (accepted) resolve();
          else reject(new Error(reason));
        }
      };
      const postControl = (message: Record<string, unknown>): boolean => {
        try {
          if (!controlPort || settled) return false;
          controlPort.postMessage(message);
          return true;
        } catch {
          finish(false, 'TS render owner control post failed');
          return false;
        }
      };
      const configureFrame = (
        source: Record<string, unknown>,
        sandbox: string
      ): HTMLIFrameElement => {
        const width = source['width'] as number;
        const height = source['height'] as number;
        const next = creativeWindow.document.createElement('iframe');
        next.setAttribute('sandbox', sandbox);
        next.setAttribute('referrerpolicy', 'no-referrer');
        next.setAttribute('width', String(width));
        next.setAttribute('height', String(height));
        next.setAttribute('scrolling', 'no');
        next.setAttribute('frameborder', '0');
        next.setAttribute('marginwidth', '0');
        next.setAttribute('marginheight', '0');
        next.setAttribute('title', 'Ad content');
        next.setAttribute('aria-label', 'Advertisement');
        next.setAttribute(
          'style',
          `border: 0; margin: 0; overflow: hidden; display: block; width: ${width}px; height: ${height}px;`
        );
        return next;
      };
      const prepareDocument = (): void => {
        const document = creativeWindow.document;
        document.documentElement.style.margin = '0';
        document.documentElement.style.padding = '0';
        document.documentElement.style.overflow = 'hidden';
        if (document.body) {
          document.body.style.margin = '0';
          document.body.style.padding = '0';
          document.body.style.overflow = 'hidden';
        }
      };
      const insertAdm = (source: Record<string, unknown>): void => {
        if (!validAdmSource(source) || !creativeWindow.document.body) {
          finish(false, 'TS ADM source refused');
          return;
        }
        prepareDocument();
        const next = configureFrame(source, admSandbox);
        const intendedSource = `<!doctype html><html><head><meta charset="utf-8"><meta name="referrer" content="no-referrer"><style>html,body{border:0;margin:0;padding:0;overflow:hidden}</style></head><body>${source['adm'] as string}</body></html>`;
        ownerFrameCurrent = () =>
          frame === next &&
          next.parentNode === creativeWindow.document.body &&
          next.srcdoc === intendedSource &&
          next.getAttribute('src') === null;
        next.onload = () => {
          if (!settled && ownerFrameCurrent?.() === true) {
            postControl({
              message: 'TS ADM Loaded',
              version: 1,
              lifecycleTicket,
            });
          }
        };
        next.onerror = () => {
          if (!settled && frame === next) {
            postControl({
              message: 'TS ADM Failed',
              version: 1,
              lifecycleTicket,
            });
          }
        };
        next.srcdoc = intendedSource;
        frame = next;
        creativeWindow.document.body.appendChild(next);
        postControl({
          message: 'TS Owner Inserted',
          version: 1,
          lifecycleTicket,
        });
      };
      const insertAps = (start: Record<string, unknown>, ports: MessagePort[]): void => {
        const envelope = exactRecord(start['envelope'], [
          'version',
          'nonce',
          'publisherOrigin',
          'renderer',
        ]);
        const rendererCandidate = envelope?.['renderer'];
        const renderer = envelope
          ? exactRecord(rendererCandidate, [
              'type',
              'version',
              'accountId',
              'bidId',
              'tagType',
              'creativeUrl',
              'width',
              'height',
              'aaxResponse',
              ...(typeof rendererCandidate === 'object' &&
              rendererCandidate !== null &&
              Object.prototype.hasOwnProperty.call(rendererCandidate, 'creativeId')
                ? ['creativeId']
                : []),
            ])
          : undefined;
        const rendererUrl = start['rendererUrl'];
        let parsedUrl: URL | undefined;
        let parsedPublisherOrigin: URL | undefined;
        try {
          parsedUrl = typeof rendererUrl === 'string' ? new URL(rendererUrl) : undefined;
          parsedPublisherOrigin =
            typeof envelope?.['publisherOrigin'] === 'string'
              ? new URL(envelope['publisherOrigin'])
              : undefined;
        } catch {
          parsedUrl = undefined;
          parsedPublisherOrigin = undefined;
        }
        if (
          !envelope ||
          !renderer ||
          envelope['version'] !== 1 ||
          typeof envelope['nonce'] !== 'string' ||
          !/^n1_[A-Za-z0-9_-]{22}$/.test(envelope['nonce']) ||
          typeof envelope['publisherOrigin'] !== 'string' ||
          new TextEncoder().encode(envelope['publisherOrigin']).byteLength > 2048 ||
          !parsedUrl ||
          !parsedPublisherOrigin ||
          !validApsRenderer(renderer, parsedPublisherOrigin) ||
          new TextEncoder().encode(String(rendererUrl)).byteLength > 2048 ||
          (parsedUrl.protocol !== 'https:' && parsedUrl.protocol !== 'http:') ||
          parsedUrl.hostname === '' ||
          parsedUrl.username !== '' ||
          parsedUrl.password !== '' ||
          parsedUrl.pathname !== '/integrations/aps/renderer/v1' ||
          parsedUrl.search !== '' ||
          parsedUrl.hash !== '' ||
          (parsedPublisherOrigin.protocol !== 'https:' &&
            parsedPublisherOrigin.protocol !== 'http:') ||
          parsedPublisherOrigin.hostname === '' ||
          parsedPublisherOrigin.username !== '' ||
          parsedPublisherOrigin.password !== '' ||
          parsedPublisherOrigin.origin !== envelope['publisherOrigin'] ||
          parsedPublisherOrigin.pathname !== '/' ||
          parsedPublisherOrigin.search !== '' ||
          parsedPublisherOrigin.hash !== '' ||
          parsedUrl.origin !== parsedPublisherOrigin.origin ||
          ports.length !== 1 ||
          !creativeWindow.document.body
        ) {
          closePort(ports[0]);
          finish(false, 'TS APS start refused');
          return;
        }
        prepareDocument();
        documentPort = ports[0];
        const next = configureFrame(renderer, apsSandbox);
        const intendedSource = `${parsedUrl.href}#tsaps=${envelope['nonce'] as string}`;
        let intendedWindow: Window | null = null;
        ownerFrameCurrent = () =>
          frame === next &&
          next.parentNode === creativeWindow.document.body &&
          next.getAttribute('src') === intendedSource &&
          next.contentWindow === intendedWindow;
        const containLocalFailure = (transferred?: MessagePort): void => {
          localApsFailure = true;
          try {
            next.onload = null;
          } catch {
            // Local containment continues through hostile DOM setters.
          }
          try {
            next.onerror = null;
          } catch {
            // Local containment continues through hostile DOM setters.
          }
          closePort(transferred);
          if (documentPort) {
            closePort(documentPort);
            documentPort = undefined;
          }
          removeFrame(next);
        };
        next.onload = () => {
          if (settled || ownerFrameCurrent?.() !== true || !documentPort) return;
          const transferred = documentPort;
          documentPort = undefined;
          try {
            const target = next.contentWindow;
            if (!target) throw new Error('APS document target is unavailable');
            target.postMessage(envelope, '*', [transferred]);
          } catch {
            containLocalFailure(transferred);
          }
        };
        next.onerror = () => containLocalFailure();
        next.src = intendedSource;
        frame = next;
        creativeWindow.document.body.appendChild(next);
        intendedWindow = next.contentWindow;
        postControl({
          message: 'TS Owner Inserted',
          version: 1,
          lifecycleTicket,
        });
      };
      const receiveControl = (event: MessageEvent): void => {
        if (settled) {
          closeEventPorts(event);
          return;
        }
        const ports = eventPorts(event, 0) ?? eventPorts(event, 1);
        const dataValue = eventDataValue(event);
        const routedMessage = ownDataValue(dataValue, 'message');
        const routedOutcome = ownDataValue(dataValue, 'outcome');
        const message = exactRecord(dataValue, [
          'message',
          'version',
          'lifecycleTicket',
          ...(routedMessage === 'TS APS Start'
            ? ['rendererUrl', 'envelope']
            : routedMessage === 'TS ADM Start'
              ? ['source']
              : routedMessage === 'TS Owner Settled' && routedOutcome !== undefined
                ? routedOutcome === 'accepted'
                  ? ['outcome']
                  : ['outcome', 'reason']
                : []),
        ]);
        if (
          !message ||
          !ports ||
          message['version'] !== 1 ||
          message['lifecycleTicket'] !== lifecycleTicket
        ) {
          closeEventPorts(event);
          finish(false, 'TS render owner control refused');
          return;
        }
        if (
          message['message'] === 'TS ADM Start' &&
          ownerKind === 'adm' &&
          ports.length === 0 &&
          !started
        ) {
          started = true;
          insertAdm(message['source'] as Record<string, unknown>);
          return;
        }
        if (
          message['message'] === 'TS APS Start' &&
          ownerKind === 'aps' &&
          ports.length === 1 &&
          !started
        ) {
          started = true;
          insertAps(message, ports);
          return;
        }
        if (message['message'] === 'TS Owner Settled' && ports.length === 0) {
          if (
            message['outcome'] === 'accepted' &&
            !localApsFailure &&
            ownerFrameCurrent?.() === true
          ) {
            frameCommitted = true;
            finish(true, '');
            return;
          }
          if (
            message['outcome'] === 'failed' &&
            typeof message['reason'] === 'string' &&
            renderFailureReasons.has(message['reason'])
          ) {
            finish(false, message['reason']);
            return;
          }
          if (
            message['outcome'] === 'cancelled' &&
            typeof message['reason'] === 'string' &&
            cancellationReasons.has(message['reason'])
          ) {
            finish(false, String(message['reason']));
            return;
          }
        }
        closeEventPorts(event);
        finish(false, 'TS render owner control refused');
      };
      const receiveRegistration = (event: unknown): void => {
        if (settled || registrationFinished) {
          closeEventPorts(event);
          return;
        }
        registrationFinished = true;
        stopHelper();
        clearTimer(registrationTimer);
        const ports = eventPorts(event, 1);
        const dataValue = eventDataValue(event);
        const response = parseRegistration(dataValue);
        if (
          !ports ||
          !response ||
          response['message'] !== 'TS Render Owner Registered' ||
          response['adId'] !== adId ||
          response['version'] !== 1 ||
          response['lifecycleTicket'] !== lifecycleTicket
        ) {
          closeEventPorts(event);
          finish(false, 'TS render owner registration refused');
          return;
        }
        const registeredPort = ports[0];
        if (!registeredPort) {
          finish(false, 'TS render owner registration refused');
          return;
        }
        controlPort = registeredPort;
        ownerTimer = creativeWindow.setTimeout(
          () => finish(false, 'TS render owner settlement timeout'),
          20_000
        );
        registeredPort.onmessage = receiveControl;
        registeredPort.onmessageerror = () => finish(false, 'TS render owner channel failed');
        try {
          registeredPort.start();
        } catch {
          finish(false, 'TS render owner channel failed');
        }
      };

      const registrationTimer = creativeWindow.setTimeout(
        () => finish(false, 'TS render owner registration timeout'),
        3_000
      );
      try {
        const disposer = Reflect.apply(sendMessage, helper, [
          'TS Render Owner Register',
          { version: 1, lifecycleTicket },
          receiveRegistration,
        ]) as unknown;
        if (typeof disposer !== 'function') {
          finish(false, 'TS render owner registration failed');
          return;
        }
        helperDisposer = disposer as () => void;
        if (registrationFinished) stopHelper();
      } catch {
        finish(false, 'TS render owner registration failed');
      }
    });
}

/** Exact checked-in program returned through PUC's dynamic renderer field. */
export const PUC_DYNAMIC_OWNER = `(${String(installPucDynamicOwner)})();`;

interface PendingClaim {
  readonly port: MessagingPort;
  readonly source: object;
}

export interface PucRenderAttempt {
  readonly id: string;
  readonly slot: string;
  readonly generation: object;
  readonly navigationGeneration: object;
  readonly renderSource: ReservationRenderSource | undefined;
  readonly beginGamClaim: () => boolean;
  readonly admitClaimedWinner: (claim: unknown) => boolean;
  readonly ownerClaimed: () => boolean;
  readonly ownerRegistered: () => boolean;
  readonly beginApsDocument: (artifact: CommittedRenderArtifact) => boolean;
  readonly beginAdm: (artifact: CommittedRenderArtifact) => boolean;
  readonly apsDocumentAccepted: () => boolean;
  readonly accept: () => boolean;
  readonly cancel: (reason: 'caller_aborted' | 'superseded' | 'navigation_disposed') => boolean;
  readonly fail: (reason: RenderFailureReason) => boolean;
  readonly onSettled: (callback: (outcome: RenderOutcome) => void) => boolean;
  readonly snapshot: () => Readonly<{
    state: string;
    outcome: Readonly<object> | undefined;
  }>;
}

export interface PucGamAttemptInput {
  readonly attempt: PucRenderAttempt;
  readonly artifact: CommittedRenderArtifact;
  readonly owner: ReservationAttempt &
    Readonly<{ generation: object; navigationGeneration: object }>;
  readonly reservationId: string;
}

export interface PucBridgeScheduler {
  readonly set: (callback: () => void, milliseconds: number) => unknown;
  readonly clear: (handle: unknown) => void;
}

export interface PucBridgeOptions {
  readonly messaging: MessagingAdapter;
  readonly reservations: Pick<ReservationService, 'claim' | 'recognize'> &
    Partial<Pick<ReservationService, 'tombstone'>>;
  readonly mintLifecycleTicket: () => IdentityGenerationResult<string>;
  readonly now?: () => number;
  readonly publisherOrigin?: string;
  readonly resizeCollapsedShell?: (input: CollapsedPucShellResizeInput) => boolean;
  readonly rendererNonces?: Pick<RendererNonceRegistry, 'consume' | 'issue'>;
  readonly rendererUrl?: string;
  readonly scheduler?: PucBridgeScheduler;
}

export interface PucBridgeInventory {
  readonly attempts: number;
  readonly disposed: boolean;
  readonly liveTickets: number;
  readonly pendingClaims: number;
  readonly ticketTombstones: number;
}

export interface PucBridge {
  registerGamAttempt(input: PucGamAttemptInput): boolean;
  recordNonemptyGam(input: PucGamAttemptInput): boolean;
  dispose(): void;
  snapshotInventoryForTest(): PucBridgeInventory;
}

interface GamAttemptBinding {
  readonly reservationId: string;
  readonly attempt: PucRenderAttempt;
  readonly artifact: CommittedRenderArtifact;
  readonly owner: PucGamAttemptInput['owner'];
  readonly attemptId: string;
  readonly slot: string;
  readonly navigationGeneration: object;
  artifactOwned: boolean;
  active: boolean;
  claim: PendingClaim | undefined;
  claimDeadlineHandle: unknown;
  claimDeadlineToken: object | undefined;
  controlListenerDispose: (() => void) | undefined;
  controlPort: MessagingPort | undefined;
  controlStarted: boolean;
  documentAccepted: boolean;
  documentAcceptancePending: boolean;
  documentTerminalPending: 'completed' | RenderFailureReason | undefined;
  documentListenerDispose: (() => void) | undefined;
  documentPort: MessagingPort | undefined;
  documentPortRegistryOwned: boolean;
  documentTransferredPort: MessagingPort | undefined;
  gamReady: boolean;
  joining: boolean;
  lifecycleTicket: string | undefined;
  nonce: string | undefined;
  ownerInserted: boolean;
  pucSource: object | undefined;
  ticket: string | undefined;
}

interface LiveTicket {
  readonly state: 'live';
  readonly binding: GamAttemptBinding;
  readonly expiresAt: number;
  expiryHandle: unknown;
}

interface PendingTicket {
  readonly state: 'pending';
  readonly binding: GamAttemptBinding;
}

interface TicketTombstone {
  readonly state: 'tombstone';
  readonly expiresAt: number;
  expiryHandle: unknown;
}

type TicketEntry = LiveTicket | PendingTicket | TicketTombstone;

function mapValue<Key, Value>(map: Map<Key, Value>, key: Key): Value | undefined {
  return Reflect.apply(mapGetIntrinsic, map, [key]) as Value | undefined;
}

function setMapValue<Key, Value>(map: Map<Key, Value>, key: Key, value: Value): void {
  Reflect.apply(mapSetIntrinsic, map, [key, value]);
}

function deleteMapValue<Key, Value>(map: Map<Key, Value>, key: Key): boolean {
  return Reflect.apply(mapDeleteIntrinsic, map, [key]) as boolean;
}

function mapSize<Key, Value>(map: Map<Key, Value>): number {
  return Reflect.apply(mapSizeGetter, map, []) as number;
}

function snapshotMapEntries<Key, Value>(map: Map<Key, Value>): readonly [Key, Value][] {
  const iterator = Reflect.apply(mapEntriesIntrinsic, map, []) as IterableIterator<[Key, Value]>;
  const entries: Array<[Key, Value]> = [];
  while (true) {
    const step = Reflect.apply(mapEntryIteratorNextIntrinsic, iterator, []) as IteratorResult<
      [Key, Value]
    >;
    if (step.done) return entries;
    entries[entries.length] = step.value;
  }
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

function utf8Length(value: string): number {
  return (Reflect.apply(textEncoderEncodeIntrinsic, textEncoder, [value]) as Uint8Array).byteLength;
}

function defaultNow(): number {
  return Date.now();
}

function defaultScheduler(): PucBridgeScheduler {
  return frozen({
    set: (callback: () => void, milliseconds: number): unknown =>
      globalThis.setTimeout(callback, milliseconds),
    clear: (handle: unknown): void => {
      globalThis.clearTimeout(handle as ReturnType<typeof globalThis.setTimeout>);
    },
  });
}

function validTicket(value: unknown): value is string {
  return typeof value === 'string' && LIFECYCLE_TICKET.test(value);
}

function readMintedTicket(value: unknown): string | undefined {
  try {
    if (typeof value !== 'object' || value === null || !Object.isFrozen(value)) return undefined;
    if (Object.getPrototypeOf(value) !== Object.prototype) return undefined;
    if (Object.getOwnPropertySymbols(value).length !== 0) return undefined;
    const names = Object.getOwnPropertyNames(value).sort();
    const ok = Object.getOwnPropertyDescriptor(value, 'ok');
    if (!ok || !ok.enumerable || !('value' in ok)) return undefined;
    if (ok.value === true && names.length === 2 && names[0] === 'ok' && names[1] === 'value') {
      const ticket = Object.getOwnPropertyDescriptor(value, 'value');
      return ticket && ticket.enumerable && 'value' in ticket && validTicket(ticket.value)
        ? ticket.value
        : undefined;
    }
    return undefined;
  } catch {
    return undefined;
  }
}

function readRendererNonceIssue(value: unknown):
  | Readonly<{ ok: true; nonce: string }>
  | Readonly<{
      ok: false;
      reason: 'capability_registry_full' | 'identity_generation_failed' | 'invalid_attempt';
    }>
  | undefined {
  try {
    if (
      typeof value !== 'object' ||
      value === null ||
      !Object.isFrozen(value) ||
      Object.getPrototypeOf(value) !== Object.prototype ||
      Object.getOwnPropertySymbols(value).length !== 0
    ) {
      return undefined;
    }
    const names = Object.getOwnPropertyNames(value).sort();
    if (names.length !== 2) return undefined;
    const ok = Object.getOwnPropertyDescriptor(value, 'ok');
    if (!ok || !ok.enumerable || !('value' in ok)) return undefined;
    if (ok.value === true && names[0] === 'nonce' && names[1] === 'ok') {
      const nonce = Object.getOwnPropertyDescriptor(value, 'nonce');
      return nonce &&
        nonce.enumerable &&
        'value' in nonce &&
        typeof nonce.value === 'string' &&
        /^n1_[A-Za-z0-9_-]{22}$/.test(nonce.value)
        ? frozen({ ok: true as const, nonce: nonce.value })
        : undefined;
    }
    if (ok.value === false && names[0] === 'ok' && names[1] === 'reason') {
      const reason = Object.getOwnPropertyDescriptor(value, 'reason');
      if (
        reason &&
        reason.enumerable &&
        'value' in reason &&
        (reason.value === 'capability_registry_full' ||
          reason.value === 'identity_generation_failed' ||
          reason.value === 'invalid_attempt')
      ) {
        return frozen({ ok: false as const, reason: reason.value });
      }
    }
    return undefined;
  } catch {
    return undefined;
  }
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

function closePort(port: MessagingPort): void {
  try {
    port.close();
  } catch {
    // The adapter facade contains raw close failures and is exact-once.
  }
}

function closeChannel(
  channel: Readonly<{ retained: MessagingPort; transferred: MessagingPort }>
): void {
  closePort(channel.retained);
  closePort(channel.transferred);
}

function readyResponse(
  adId: string,
  dynamicOwner: string,
  kind: 'aps' | 'adm',
  lifecycleTicket: string
): string | undefined {
  try {
    const owner = Object.create(null) as Record<string, unknown>;
    owner['version'] = 1;
    owner['status'] = TSJS_MESSAGE_PROTOCOL_V1.status.ready;
    owner['kind'] = kind;
    owner['lifecycleTicket'] = lifecycleTicket;
    const response = Object.create(null) as Record<string, unknown>;
    response['message'] = TSJS_MESSAGE_PROTOCOL_V1.message.prebidResponse;
    response['adId'] = adId;
    response['renderer'] = dynamicOwner;
    response['rendererVersion'] = TSJS_MESSAGE_PROTOCOL_V1.rendererVersion;
    response['tsOwner'] = owner;
    const serialized = Reflect.apply(jsonStringifyIntrinsic, JSON, [response]) as unknown;
    return typeof serialized === 'string' && utf8Length(serialized) <= MAX_OUTER_RESPONSE_BYTES
      ? serialized
      : undefined;
  } catch {
    return undefined;
  }
}

function ownerResponse(adId: string, lifecycleTicket: string | undefined): string | undefined {
  try {
    const response = Object.create(null) as Record<string, unknown>;
    response['message'] =
      lifecycleTicket === undefined
        ? TSJS_MESSAGE_PROTOCOL_V1.message.ownerRefused
        : TSJS_MESSAGE_PROTOCOL_V1.message.ownerRegistered;
    response['adId'] = adId;
    response['version'] = 1;
    if (lifecycleTicket !== undefined) response['lifecycleTicket'] = lifecycleTicket;
    const serialized = Reflect.apply(jsonStringifyIntrinsic, JSON, [response]) as unknown;
    return typeof serialized === 'string' ? serialized : undefined;
  } catch {
    return undefined;
  }
}

function ownerSettlement(
  lifecycleTicket: string,
  outcome: RenderOutcome
): Readonly<Record<string, unknown>> | undefined {
  try {
    const message = Object.create(null) as Record<string, unknown>;
    message['message'] = TSJS_MESSAGE_PROTOCOL_V1.message.ownerSettled;
    message['version'] = 1;
    message['lifecycleTicket'] = lifecycleTicket;
    if (outcome.outcome === 'accepted') {
      message['outcome'] = TSJS_MESSAGE_PROTOCOL_V1.outcome.accepted;
      return frozen(message);
    }
    if (outcome.outcome === 'failed') {
      message['outcome'] = TSJS_MESSAGE_PROTOCOL_V1.outcome.failed;
      message['reason'] = outcome.reason;
      return frozen(message);
    }
    if (outcome.outcome === 'cancelled') {
      message['outcome'] = TSJS_MESSAGE_PROTOCOL_V1.outcome.cancelled;
      message['reason'] = outcome.reason;
      return frozen(message);
    }
    return undefined;
  } catch {
    return undefined;
  }
}

function refuseOwner(port: MessagingPort, adId: string): void {
  try {
    const response = ownerResponse(adId, undefined);
    if (response !== undefined) port.post(response, []);
  } catch {
    // Refusal transport is best-effort; endpoint closure remains mandatory.
  } finally {
    closePort(port);
  }
}

/**
 * Own the runtime-wide Universal Creative capture dispatcher.
 *
 * Request recognition deliberately precedes exact parsing and port inspection so
 * malformed or replayed TS capabilities cannot fall through to native Prebid.
 */
export function createPucBridge(options: PucBridgeOptions): PucBridge {
  let messaging: MessagingAdapter;
  let reservations: PucBridgeOptions['reservations'];
  let mintLifecycleTicket: () => IdentityGenerationResult<string>;
  let nowSource: () => number;
  let publisherOrigin: string | undefined;
  let rendererNonces: PucBridgeOptions['rendererNonces'];
  let rendererUrl: string | undefined;
  let resizeCollapsedShell: PucBridgeOptions['resizeCollapsedShell'];
  let scheduler: PucBridgeScheduler;
  try {
    messaging = options.messaging;
    reservations = options.reservations;
    mintLifecycleTicket = options.mintLifecycleTicket;
    nowSource = options.now ?? defaultNow;
    publisherOrigin = options.publisherOrigin;
    rendererNonces = options.rendererNonces;
    rendererUrl = options.rendererUrl;
    resizeCollapsedShell = options.resizeCollapsedShell;
    scheduler = options.scheduler ?? defaultScheduler();
  } catch {
    messaging = options.messaging;
    reservations = options.reservations;
    mintLifecycleTicket = () => frozen({ ok: false, reason: 'identity_generation_failed' });
    nowSource = () => Number.NaN;
    publisherOrigin = undefined;
    rendererNonces = undefined;
    rendererUrl = undefined;
    resizeCollapsedShell = undefined;
    scheduler = defaultScheduler();
  }
  let schedulerSet: PucBridgeScheduler['set'];
  let schedulerClear: PucBridgeScheduler['clear'];
  try {
    schedulerSet = scheduler.set;
    schedulerClear = scheduler.clear;
  } catch {
    schedulerSet = () => undefined;
    schedulerClear = () => undefined;
  }
  const dynamicOwnerValid = utf8Length(PUC_DYNAMIC_OWNER) <= MAX_DYNAMIC_OWNER_BYTES;
  const attempts = new Map<string, GamAttemptBinding>();
  const tickets = new Map<string, TicketEntry>();
  let pendingTicketIssues = 0;
  let lastNow = Number.NEGATIVE_INFINITY;
  let disposed = false;

  const readNow = (): number | undefined => {
    try {
      const value = Reflect.apply(nowSource, undefined, []) as number;
      if (!Number.isFinite(value) || value < 0 || value < lastNow) return undefined;
      lastNow = value;
      return value;
    } catch {
      return undefined;
    }
  };

  const clearScheduled = (handle: unknown): void => {
    if (handle === undefined) return;
    try {
      Reflect.apply(schedulerClear, scheduler, [handle]);
    } catch {
      // Timer cleanup is best-effort after state is already made inert.
    }
  };

  const exactInput = (input: PucGamAttemptInput): PucGamAttemptInput | undefined => {
    try {
      const reservationId = input.reservationId;
      const attempt = input.attempt;
      const artifact = input.artifact;
      const owner = input.owner;
      if (
        typeof reservationId !== 'string' ||
        !RESERVATION_ID.test(reservationId) ||
        !ATTEMPT_ID.test(attempt.id) ||
        attempt.id !== owner.id ||
        attempt.slot !== owner.slot ||
        attempt.generation !== owner.generation ||
        attempt.navigationGeneration !== owner.navigationGeneration ||
        artifact.kind !== 'puc' ||
        artifact.attemptId !== attempt.id ||
        artifact.slot !== owner.slot ||
        artifact.navigationGeneration !== owner.navigationGeneration ||
        typeof artifact.dispose !== 'function' ||
        typeof attempt.beginGamClaim !== 'function' ||
        typeof attempt.admitClaimedWinner !== 'function' ||
        typeof attempt.ownerClaimed !== 'function' ||
        typeof attempt.ownerRegistered !== 'function' ||
        typeof attempt.beginApsDocument !== 'function' ||
        typeof attempt.beginAdm !== 'function' ||
        typeof attempt.apsDocumentAccepted !== 'function' ||
        typeof attempt.accept !== 'function' ||
        typeof attempt.cancel !== 'function' ||
        typeof attempt.fail !== 'function' ||
        typeof attempt.onSettled !== 'function' ||
        typeof attempt.snapshot !== 'function' ||
        typeof owner.isCurrent !== 'function' ||
        typeof owner.prepareWinnerContext !== 'function'
      ) {
        return undefined;
      }
      return frozen({ reservationId, attempt, artifact, owner });
    } catch {
      return undefined;
    }
  };

  const bindingMatches = (binding: GamAttemptBinding, input: PucGamAttemptInput): boolean =>
    binding.active &&
    binding.reservationId === input.reservationId &&
    binding.attempt === input.attempt &&
    binding.artifact === input.artifact &&
    binding.owner === input.owner &&
    binding.attemptId === input.attempt.id &&
    binding.slot === input.owner.slot &&
    binding.navigationGeneration === input.owner.navigationGeneration;

  const currentBindingState = (binding: GamAttemptBinding, expectedState: string): boolean => {
    try {
      const snapshot = Reflect.apply(binding.attempt.snapshot, binding.attempt, []);
      return (
        binding.active &&
        mapValue(attempts, binding.reservationId) === binding &&
        binding.attempt.id === binding.attemptId &&
        binding.attempt.slot === binding.slot &&
        binding.attempt.navigationGeneration === binding.navigationGeneration &&
        binding.owner.id === binding.attemptId &&
        binding.owner.slot === binding.slot &&
        binding.owner.navigationGeneration === binding.navigationGeneration &&
        Reflect.apply(binding.owner.isCurrent, binding.owner, []) === true &&
        snapshot.outcome === undefined &&
        snapshot.state === expectedState
      );
    } catch {
      return false;
    }
  };

  const tombstoneReservation = (binding: GamAttemptBinding): void => {
    try {
      const tombstone = reservations.tombstone;
      if (typeof tombstone !== 'function') return;
      Reflect.apply(tombstone, reservations, [
        frozen({
          reservationId: binding.reservationId,
          slot: binding.slot,
          navigationGeneration: binding.navigationGeneration,
          attemptId: binding.attemptId,
        }),
        'stale',
      ]);
    } catch {
      // Attempt settlement stays authoritative if suppression publication fails.
    }
  };

  const retireTicket = (binding: GamAttemptBinding): void => {
    const ticket = binding.ticket;
    if (!ticket) return;
    const entry = mapValue(tickets, ticket);
    if (entry?.state === 'live' && entry.binding === binding) {
      clearScheduled(entry.expiryHandle);
      const tombstone: TicketTombstone = {
        state: 'tombstone',
        expiresAt: entry.expiresAt,
        expiryHandle: undefined,
      };
      setMapValue(tickets, ticket, tombstone);
      const retiredAt = readNow();
      if (retiredAt !== undefined && retiredAt >= tombstone.expiresAt) {
        expireTicket(ticket, tombstone, retiredAt);
      } else if (retiredAt !== undefined) {
        let handle: unknown;
        try {
          handle = Reflect.apply(schedulerSet, scheduler, [
            () => expireTicket(ticket, tombstone),
            tombstone.expiresAt - retiredAt,
          ]);
        } catch {
          handle = undefined;
        }
        if (mapValue(tickets, ticket) !== tombstone) clearScheduled(handle);
        else tombstone.expiryHandle = handle;
      }
    } else if (entry?.state === 'pending' && entry.binding === binding) {
      const retiredAt = readNow();
      if (retiredAt === undefined) {
        deleteMapValue(tickets, ticket);
      } else {
        const tombstone: TicketTombstone = {
          state: 'tombstone',
          expiresAt: retiredAt + LIFECYCLE_TICKET_TTL_MS,
          expiryHandle: undefined,
        };
        setMapValue(tickets, ticket, tombstone);
        let handle: unknown;
        try {
          handle = Reflect.apply(schedulerSet, scheduler, [
            () => expireTicket(ticket, tombstone),
            LIFECYCLE_TICKET_TTL_MS,
          ]);
        } catch {
          handle = undefined;
        }
        if (mapValue(tickets, ticket) !== tombstone) {
          clearScheduled(handle);
        } else {
          tombstone.expiryHandle = handle;
        }
      }
    }
    binding.ticket = undefined;
  };

  const clearClaimDeadline = (binding: GamAttemptBinding): void => {
    const handle = binding.claimDeadlineHandle;
    binding.claimDeadlineHandle = undefined;
    binding.claimDeadlineToken = undefined;
    clearScheduled(handle);
  };

  const disposeOwnedArtifact = (binding: GamAttemptBinding): void => {
    if (!binding.artifactOwned) return;
    binding.artifactOwned = false;
    try {
      Reflect.apply(binding.artifact.dispose, binding.artifact, []);
    } catch {
      // The bridge has already relinquished authority; disposal remains exact-once.
    }
  };

  const cleanupBinding = (binding: GamAttemptBinding): void => {
    if (!binding.active) return;
    binding.active = false;
    binding.joining = false;
    disposeOwnedArtifact(binding);
    clearClaimDeadline(binding);
    if (mapValue(attempts, binding.reservationId) === binding) {
      deleteMapValue(attempts, binding.reservationId);
    }
    const claim = binding.claim;
    binding.claim = undefined;
    if (claim) closePort(claim.port);
    const disposeControlListener = binding.controlListenerDispose;
    binding.controlListenerDispose = undefined;
    if (disposeControlListener) {
      try {
        disposeControlListener();
      } catch {
        // Listener disposal is best-effort after the binding is already inert.
      }
    }
    const disposeDocumentListener = binding.documentListenerDispose;
    binding.documentListenerDispose = undefined;
    if (disposeDocumentListener) {
      try {
        disposeDocumentListener();
      } catch {
        // Document listener disposal cannot interrupt endpoint cleanup.
      }
    }
    const documentPort = binding.documentPort;
    binding.documentPort = undefined;
    if (documentPort && !binding.documentPortRegistryOwned) closePort(documentPort);
    binding.documentPortRegistryOwned = false;
    const documentTransferredPort = binding.documentTransferredPort;
    binding.documentTransferredPort = undefined;
    if (documentTransferredPort) closePort(documentTransferredPort);
    const controlPort = binding.controlPort;
    binding.controlPort = undefined;
    if (controlPort) closePort(controlPort);
    binding.lifecycleTicket = undefined;
    binding.nonce = undefined;
    binding.pucSource = undefined;
    retireTicket(binding);
    tombstoneReservation(binding);
  };

  const failBinding = (
    binding: GamAttemptBinding,
    reason: RenderFailureReason,
    refusePending: boolean
  ): void => {
    if (!binding.active) return;
    if (binding.controlPort && binding.lifecycleTicket) {
      try {
        Reflect.apply(binding.attempt.fail, binding.attempt, [reason]);
      } catch {
        // The binding cleanup below remains authoritative.
      }
      if (binding.active) cleanupBinding(binding);
      return;
    }
    const claim = binding.claim;
    binding.claim = undefined;
    if (claim) {
      if (refusePending) refuse(claim.port, binding.reservationId);
      else closePort(claim.port);
    }
    cleanupBinding(binding);
    try {
      Reflect.apply(binding.attempt.fail, binding.attempt, [reason]);
    } catch {
      // The binding is already inert and all owned endpoints are closed.
    }
  };

  const settleBinding = (binding: GamAttemptBinding, outcome: RenderOutcome): void => {
    if (!binding.active) return;
    const controlPort = binding.controlPort;
    const lifecycleTicket = binding.lifecycleTicket;
    if (controlPort && lifecycleTicket) {
      const settlement = ownerSettlement(lifecycleTicket, outcome);
      if (settlement) {
        try {
          controlPort.post(settlement, []);
        } catch {
          // The remote owner's fixed watchdog contains settlement transport loss.
        }
      }
    }
    cleanupBinding(binding);
  };

  const expireTicket = (
    ticket: string,
    expected: LiveTicket | TicketTombstone,
    observedAt?: number
  ): void => {
    const entry = mapValue(tickets, ticket);
    if (entry !== expected) return;
    const now = observedAt ?? readNow();
    if (now === undefined || now < expected.expiresAt) return;
    deleteMapValue(tickets, ticket);
    if (entry.state === 'live') {
      entry.binding.ticket = undefined;
      if (entry.binding.active) {
        failBinding(entry.binding, 'owner_registration_timeout', false);
      }
    }
  };

  const pruneExpiredTickets = (now: number): void => {
    const entries = snapshotMapEntries(tickets);
    for (let index = 0; index < entries.length; index += 1) {
      const pair = entries[index];
      if (
        pair &&
        pair[1].state !== 'pending' &&
        pair[1].expiresAt <= now &&
        mapValue(tickets, pair[0]) === pair[1]
      ) {
        clearScheduled(pair[1].expiryHandle);
        expireTicket(pair[0], pair[1], now);
      }
    }
  };

  const issueTicket = (
    binding: GamAttemptBinding
  ):
    | Readonly<{ ok: true; ticket: string }>
    | Readonly<{ ok: false; reason: RenderFailureReason }> => {
    const failure = (
      reason: RenderFailureReason
    ): Readonly<{ ok: false; reason: RenderFailureReason }> =>
      frozen({ ok: false as const, reason });
    const pruneAt = readNow();
    if (pruneAt === undefined) return failure('identity_generation_failed');
    pruneExpiredTickets(pruneAt);
    if (mapSize(tickets) + pendingTicketIssues >= MAX_TICKETS) {
      return failure('capability_registry_full');
    }
    pendingTicketIssues += 1;
    try {
      for (let draw = 0; draw < MAX_TICKET_DRAWS; draw += 1) {
        let minted: unknown;
        try {
          minted = Reflect.apply(mintLifecycleTicket, undefined, []);
        } catch {
          return failure('identity_generation_failed');
        }
        const ticket = readMintedTicket(minted);
        if (!binding.active || disposed) {
          return failure('internal_error');
        }
        if (!ticket) return failure('identity_generation_failed');
        if (mapValue(tickets, ticket) !== undefined) continue;
        if (mapSize(tickets) + pendingTicketIssues > MAX_TICKETS) {
          return failure('capability_registry_full');
        }
        const entry = frozen<PendingTicket>({ state: 'pending', binding });
        binding.ticket = ticket;
        setMapValue(tickets, ticket, entry);
        if (!binding.active || mapValue(tickets, ticket) !== entry || binding.ticket !== ticket) {
          if (mapValue(tickets, ticket) === entry) deleteMapValue(tickets, ticket);
          if (binding.ticket === ticket) binding.ticket = undefined;
          return failure('internal_error');
        }
        return frozen({ ok: true, ticket });
      }
      return failure('identity_generation_failed');
    } finally {
      pendingTicketIssues -= 1;
    }
  };

  const activateTicket = (binding: GamAttemptBinding, ticket: string): boolean => {
    const pending = mapValue(tickets, ticket);
    const postedAt = readNow();
    if (
      pending?.state !== 'pending' ||
      pending.binding !== binding ||
      binding.ticket !== ticket ||
      postedAt === undefined
    ) {
      return false;
    }
    const expiresAt = postedAt + LIFECYCLE_TICKET_TTL_MS;
    if (!Number.isFinite(expiresAt) || expiresAt <= postedAt) return false;
    const live: LiveTicket = {
      state: 'live',
      binding,
      expiresAt,
      expiryHandle: undefined,
    };
    setMapValue(tickets, ticket, live);
    let expiryHandle: unknown;
    try {
      expiryHandle = Reflect.apply(schedulerSet, scheduler, [
        () => expireTicket(ticket, live),
        LIFECYCLE_TICKET_TTL_MS,
      ]);
    } catch {
      expiryHandle = undefined;
    }
    if (
      expiryHandle === undefined ||
      !binding.active ||
      mapValue(tickets, ticket) !== live ||
      binding.ticket !== ticket
    ) {
      clearScheduled(expiryHandle);
      if (binding.active && binding.ticket === ticket && mapValue(tickets, ticket) === live) {
        setMapValue(tickets, ticket, pending);
      } else {
        if (mapValue(tickets, ticket) === live) deleteMapValue(tickets, ticket);
        if (binding.ticket === ticket) binding.ticket = undefined;
      }
      return false;
    }
    live.expiryHandle = expiryHandle;
    return true;
  };

  const join = (binding: GamAttemptBinding): boolean => {
    if (
      !binding.active ||
      !binding.gamReady ||
      !binding.claim ||
      binding.joining ||
      !currentBindingState(binding, 'waiting_for_gam_and_claim')
    ) {
      return false;
    }
    binding.joining = true;
    clearClaimDeadline(binding);
    const pending = binding.claim;
    try {
      let claimed: unknown;
      try {
        claimed = Reflect.apply(reservations.claim, reservations, [
          frozen({
            reservationId: binding.reservationId,
            slot: binding.slot,
            navigationGeneration: binding.navigationGeneration,
            attempt: binding.owner,
            pucSource: pending.source,
          }),
        ]);
      } catch {
        claimed = undefined;
      }
      let claimedSuccessfully = false;
      try {
        claimedSuccessfully =
          typeof claimed === 'object' &&
          claimed !== null &&
          (claimed as { recognized?: unknown }).recognized === true &&
          (claimed as { claimed?: unknown }).claimed === true;
      } catch {
        claimedSuccessfully = false;
      }
      if (
        !claimedSuccessfully ||
        Reflect.apply(binding.attempt.admitClaimedWinner, binding.attempt, [claimed]) !== true ||
        Reflect.apply(binding.attempt.ownerClaimed, binding.attempt, []) !== true ||
        !currentBindingState(binding, 'waiting_for_owner')
      ) {
        failBinding(binding, 'bridge_id_mismatch', true);
        return false;
      }
      binding.pucSource = pending.source;
      let kind: 'aps' | 'adm';
      try {
        const sourceType = binding.attempt.renderSource?.type;
        if (sourceType === 'aps') kind = 'aps';
        else if (sourceType === 'adm') kind = 'adm';
        else throw new Error('claimed source is unavailable');
      } catch {
        failBinding(binding, 'bridge_id_mismatch', true);
        return false;
      }
      const issued = issueTicket(binding);
      if (!issued.ok) {
        failBinding(binding, issued.reason, true);
        return false;
      }
      const response = readyResponse(binding.reservationId, PUC_DYNAMIC_OWNER, kind, issued.ticket);
      if (!response) {
        failBinding(binding, 'internal_error', true);
        return false;
      }
      binding.claim = undefined;
      let posted = false;
      try {
        posted = pending.port.post(response, []) === true;
      } catch {
        posted = false;
      }
      closePort(pending.port);
      if (!posted || !binding.active || !activateTicket(binding, issued.ticket)) {
        if (binding.active) failBinding(binding, 'internal_error', false);
        return false;
      }
      if (resizeCollapsedShell && currentBindingState(binding, 'waiting_for_owner')) {
        try {
          const renderSource = binding.attempt.renderSource;
          const width = renderSource && Reflect.get(renderSource, 'width');
          const height = renderSource && Reflect.get(renderSource, 'height');
          if (
            typeof width === 'number' &&
            typeof height === 'number' &&
            Number.isFinite(width) &&
            Number.isFinite(height) &&
            width > 0 &&
            height > 0
          ) {
            Reflect.apply(resizeCollapsedShell, undefined, [
              frozen({
                source: pending.source,
                width,
                height,
              }),
            ]);
          }
        } catch {
          // Shell correction is best-effort and cannot affect render authority.
        }
      }
      return true;
    } finally {
      if (binding.active) binding.joining = false;
    }
  };

  const armClaimDeadline = (binding: GamAttemptBinding): boolean => {
    if (!binding.active || binding.claimDeadlineToken !== undefined) return false;
    const token = frozen({});
    binding.claimDeadlineToken = token;
    let handle: unknown;
    try {
      handle = Reflect.apply(schedulerSet, scheduler, [
        () => {
          if (binding.claimDeadlineToken !== token) return;
          binding.claimDeadlineToken = undefined;
          binding.claimDeadlineHandle = undefined;
          if (
            binding.active &&
            binding.gamReady &&
            !binding.claim &&
            currentBindingState(binding, 'waiting_for_gam_and_claim')
          ) {
            failBinding(binding, 'bridge_claim_timeout', false);
          }
        },
        CLAIM_DEADLINE_MS,
      ]);
    } catch {
      handle = undefined;
    }
    if (handle === undefined || !binding.active || binding.claimDeadlineToken !== token) {
      clearScheduled(handle);
      if (binding.active && binding.claimDeadlineToken === token) {
        binding.claimDeadlineToken = undefined;
        failBinding(binding, 'internal_error', false);
      }
      return false;
    }
    binding.claimDeadlineHandle = handle;
    return true;
  };

  const startAdmOwner = (binding: GamAttemptBinding, lifecycleTicket: string): boolean => {
    const controlPort = binding.controlPort;
    if (!controlPort || binding.controlStarted || !binding.active) return false;
    let source: unknown;
    try {
      source = binding.attempt.renderSource;
    } catch {
      source = undefined;
    }
    const start = messaging.parseProtocolMessage('admStart', {
      message: TSJS_MESSAGE_PROTOCOL_V1.message.admStart,
      version: 1,
      lifecycleTicket,
      source,
    });
    if (!start) {
      failBinding(binding, 'winner_not_renderable', false);
      return false;
    }
    const receive = (event: unknown): void => {
      if (!binding.active || binding.controlPort !== controlPort || !binding.controlStarted) return;
      if (!messaging.extractTransferredPorts(event, 0)) {
        failBinding(binding, 'internal_error', false);
        return;
      }
      const data = eventData(event);
      const inserted = messaging.parseProtocolMessage('ownerInserted', data);
      if (inserted?.['lifecycleTicket'] === lifecycleTicket) {
        if (binding.ownerInserted) return;
        binding.artifactOwned = false;
        const began = (() => {
          try {
            return (
              Reflect.apply(binding.attempt.beginAdm, binding.attempt, [binding.artifact]) === true
            );
          } catch {
            return false;
          }
        })();
        if (!began) {
          if (binding.active) binding.artifactOwned = true;
          failBinding(binding, 'internal_error', false);
          return;
        }
        binding.ownerInserted = true;
        return;
      }
      const loaded = messaging.parseProtocolMessage('admLoaded', data);
      if (loaded?.['lifecycleTicket'] === lifecycleTicket) {
        if (!binding.ownerInserted) {
          failBinding(binding, 'adm_document_no_load', false);
          return;
        }
        const accepted = (() => {
          try {
            return Reflect.apply(binding.attempt.accept, binding.attempt, []) === true;
          } catch {
            return false;
          }
        })();
        if (!accepted && binding.active) failBinding(binding, 'internal_error', false);
        return;
      }
      const failed = messaging.parseProtocolMessage('admFailed', data);
      if (failed?.['lifecycleTicket'] === lifecycleTicket) {
        failBinding(binding, 'adm_document_no_load', false);
        return;
      }
      failBinding(binding, 'internal_error', false);
    };
    const receiveError = (): void => failBinding(binding, 'adm_document_no_load', false);
    try {
      binding.controlListenerDispose = controlPort.listen(receive, receiveError);
      binding.controlStarted = true;
      if (controlPort.post(start, []) !== true) {
        failBinding(binding, 'internal_error', false);
        return false;
      }
      return binding.active;
    } catch {
      failBinding(binding, 'internal_error', false);
      return false;
    }
  };

  const startApsOwner = (binding: GamAttemptBinding, lifecycleTicket: string): boolean => {
    const controlPort = binding.controlPort;
    const pucSource = binding.pucSource;
    if (
      !controlPort ||
      !pucSource ||
      binding.controlStarted ||
      !binding.active ||
      !rendererNonces ||
      typeof publisherOrigin !== 'string' ||
      typeof rendererUrl !== 'string'
    ) {
      failBinding(binding, 'internal_error', false);
      return false;
    }
    const documentChannel = messaging.createChannel();
    if (!documentChannel) {
      failBinding(binding, 'internal_error', false);
      return false;
    }
    if (
      !binding.active ||
      binding.controlPort !== controlPort ||
      !currentBindingState(binding, 'waiting_for_insertion')
    ) {
      closeChannel(documentChannel);
      return false;
    }
    binding.documentPort = documentChannel.retained;
    binding.documentTransferredPort = documentChannel.transferred;
    binding.documentPortRegistryOwned = true;
    let issuedValue: unknown;
    try {
      issuedValue = Reflect.apply(rendererNonces.issue, rendererNonces, [
        frozen({
          attempt: binding.attempt as unknown as RenderAttempt,
          source: pucSource,
          port: documentChannel.retained,
        }),
      ]);
    } catch {
      issuedValue = undefined;
    }
    const issued = readRendererNonceIssue(issuedValue);
    if (!issued?.ok) {
      binding.documentPortRegistryOwned = false;
      if (!binding.active) {
        closePort(documentChannel.retained);
        closePort(documentChannel.transferred);
        return false;
      }
      const reason = issued?.reason;
      failBinding(
        binding,
        reason === 'capability_registry_full' || reason === 'identity_generation_failed'
          ? reason
          : 'internal_error',
        false
      );
      return false;
    }
    if (!binding.active) return false;
    const nonce = issued.nonce;
    binding.nonce = nonce;
    let source: unknown;
    try {
      source = binding.attempt.renderSource;
    } catch {
      source = undefined;
    }
    const start = messaging.parseProtocolMessage('apsStart', {
      message: TSJS_MESSAGE_PROTOCOL_V1.message.apsStart,
      version: 1,
      lifecycleTicket,
      rendererUrl,
      envelope: {
        version: 1,
        nonce,
        publisherOrigin,
        renderer: source,
      },
    });
    if (!start) {
      failBinding(binding, 'winner_not_renderable', false);
      return false;
    }
    const nonceExpectation = () =>
      frozen({
        nonce,
        attempt: binding.attempt as unknown as RenderAttempt,
        generation: binding.attempt.generation,
        source: pucSource,
        port: documentChannel.retained,
      });
    const acceptDocument = (): boolean => {
      if (!binding.active || binding.documentAccepted || !binding.ownerInserted) return false;
      const advanced = (() => {
        try {
          return (
            Reflect.apply(rendererNonces.consume, rendererNonces, [nonceExpectation()]) === true &&
            Reflect.apply(binding.attempt.apsDocumentAccepted, binding.attempt, []) === true
          );
        } catch {
          return false;
        }
      })();
      if (!advanced) {
        failBinding(binding, 'renderer_document_no_load', false);
        return false;
      }
      binding.documentAcceptancePending = false;
      binding.documentAccepted = true;
      return true;
    };
    const receiveDocument = (event: unknown): void => {
      if (!binding.active || binding.documentPort !== documentChannel.retained) return;
      if (!messaging.extractTransferredPorts(event, 0)) {
        failBinding(
          binding,
          binding.documentAccepted ? 'runner_failed' : 'renderer_document_no_load',
          false
        );
        return;
      }
      const data = eventData(event);
      const accepted = messaging.parseProtocolMessage('apsDocumentAccepted', data);
      if (accepted?.['nonce'] === nonce) {
        if (binding.documentAccepted) return;
        if (!binding.ownerInserted) {
          binding.documentAcceptancePending = true;
          return;
        }
        acceptDocument();
        return;
      }
      const loaded = messaging.parseProtocolMessage('apsRunnerLoaded', data);
      if (loaded?.['nonce'] === nonce) {
        if (!binding.documentAccepted && !binding.documentAcceptancePending) {
          failBinding(binding, 'renderer_document_no_load', false);
        }
        return;
      }
      const completed = messaging.parseProtocolMessage('apsRenderCompleted', data);
      if (completed?.['nonce'] === nonce) {
        if (!binding.documentAccepted) {
          if (binding.documentAcceptancePending) {
            if (binding.documentTerminalPending === undefined) {
              binding.documentTerminalPending = 'completed';
            }
            return;
          }
          failBinding(binding, 'renderer_document_no_load', false);
          return;
        }
        const rendered = (() => {
          try {
            return Reflect.apply(binding.attempt.accept, binding.attempt, []) === true;
          } catch {
            return false;
          }
        })();
        if (!rendered && binding.active) failBinding(binding, 'internal_error', false);
        return;
      }
      const failed = messaging.parseProtocolMessage('apsRenderFailed', data);
      if (failed?.['nonce'] === nonce) {
        const reason = failed['reason'];
        const mapped =
          reason === 'descriptor_invalid' ||
          reason === 'runner_no_load' ||
          reason === 'runner_failed'
            ? reason
            : 'winner_not_renderable';
        if (!binding.documentAccepted && binding.documentAcceptancePending) {
          if (binding.documentTerminalPending === undefined) {
            binding.documentTerminalPending = mapped;
          }
          return;
        }
        failBinding(binding, mapped, false);
        return;
      }
      failBinding(
        binding,
        binding.documentAccepted ? 'runner_failed' : 'renderer_document_no_load',
        false
      );
    };
    const receiveDocumentError = (): void =>
      failBinding(
        binding,
        binding.documentAccepted ? 'runner_failed' : 'renderer_document_no_load',
        false
      );
    const receiveControl = (event: unknown): void => {
      if (!binding.active || binding.controlPort !== controlPort || !binding.controlStarted) return;
      if (!messaging.extractTransferredPorts(event, 0)) {
        failBinding(binding, 'internal_error', false);
        return;
      }
      const inserted = messaging.parseProtocolMessage('ownerInserted', eventData(event));
      if (inserted?.['lifecycleTicket'] !== lifecycleTicket) {
        failBinding(binding, 'internal_error', false);
        return;
      }
      if (binding.ownerInserted) return;
      binding.artifactOwned = false;
      const began = (() => {
        try {
          return (
            Reflect.apply(binding.attempt.beginApsDocument, binding.attempt, [binding.artifact]) ===
            true
          );
        } catch {
          return false;
        }
      })();
      if (!began) {
        if (binding.active) binding.artifactOwned = true;
        failBinding(binding, 'internal_error', false);
        return;
      }
      binding.ownerInserted = true;
      if (binding.documentAcceptancePending) {
        acceptDocument();
        if (!binding.active) return;
        const terminal = binding.documentTerminalPending;
        binding.documentTerminalPending = undefined;
        if (terminal === 'completed') {
          const rendered = (() => {
            try {
              return Reflect.apply(binding.attempt.accept, binding.attempt, []) === true;
            } catch {
              return false;
            }
          })();
          if (!rendered && binding.active) failBinding(binding, 'internal_error', false);
        } else if (terminal) {
          failBinding(binding, terminal, false);
        }
      }
    };
    const receiveControlError = (): void => failBinding(binding, 'internal_error', false);
    try {
      binding.documentListenerDispose = documentChannel.retained.listen(
        receiveDocument,
        receiveDocumentError
      );
      binding.controlListenerDispose = controlPort.listen(receiveControl, receiveControlError);
      binding.controlStarted = true;
      if (controlPort.post(start, [documentChannel.transferred]) !== true) {
        failBinding(binding, 'internal_error', false);
        return false;
      }
      closePort(documentChannel.transferred);
      return binding.active;
    } catch {
      failBinding(binding, 'internal_error', false);
      return false;
    }
  };

  const handleOwnerRegistration = (
    event: MessageEvent,
    data: unknown,
    routing: Readonly<{ message: string; adId?: string; lifecycleTicket?: string }>
  ): void => {
    const ticket = routing.lifecycleTicket;
    if (!ticket) return;
    const entry = mapValue(tickets, ticket);
    if (!entry) return;
    if (!suppress(event)) return;
    const now = readNow();
    let entryStillCurrent = true;
    if (now !== undefined) {
      pruneExpiredTickets(now);
      entryStillCurrent = mapValue(tickets, ticket) === entry;
    }

    const exact = messaging.parseProtocolMessage('ownerRegister', data);
    const inspection = messaging.inspectTransferredPorts(event);
    const ports = inspection?.ports;
    const responsePort = ports?.[0];
    const exactPort =
      inspection?.exactShape === true && inspection.originalCount === 1 && ports?.length === 1;
    const closeAdditionalPorts = (): void => {
      if (!ports) return;
      for (let index = 1; index < ports.length; index += 1) {
        const port = ports[index];
        if (port) closePort(port);
      }
    };
    if (!entryStillCurrent) {
      if (responsePort) refuseOwner(responsePort, routing.adId ?? '');
      closeAdditionalPorts();
      return;
    }
    if (now === undefined) {
      if (responsePort) refuseOwner(responsePort, routing.adId ?? '');
      closeAdditionalPorts();
      if (entry.state !== 'tombstone') {
        retireTicket(entry.binding);
        failBinding(entry.binding, 'internal_error', false);
      }
      return;
    }
    if (entry.state === 'tombstone') {
      if (responsePort) refuseOwner(responsePort, routing.adId ?? '');
      closeAdditionalPorts();
      return;
    }

    if (entry.state === 'pending') {
      if (responsePort) refuseOwner(responsePort, routing.adId ?? '');
      closeAdditionalPorts();
      retireTicket(entry.binding);
      failBinding(entry.binding, 'bridge_id_mismatch', false);
      return;
    }

    const binding = entry.binding;
    const invalidate = (): void => {
      if (responsePort) refuseOwner(responsePort, routing.adId ?? binding.reservationId);
      closeAdditionalPorts();
      retireTicket(binding);
      failBinding(binding, 'bridge_id_mismatch', false);
    };
    if (!exact || !responsePort || !exactPort) {
      invalidate();
      return;
    }
    const source = eventSource(event);
    let exactAdId: unknown;
    let exactTicket: unknown;
    try {
      exactAdId = exact['adId'];
      exactTicket = exact['lifecycleTicket'];
    } catch {
      invalidate();
      return;
    }
    if (
      source === undefined ||
      source !== binding.pucSource ||
      exactAdId !== binding.reservationId ||
      exactTicket !== ticket ||
      binding.ticket !== ticket ||
      mapValue(tickets, ticket) !== entry ||
      !currentBindingState(binding, 'waiting_for_owner')
    ) {
      invalidate();
      return;
    }

    binding.lifecycleTicket = ticket;
    retireTicket(binding);
    const channel = messaging.createChannel();
    if (!channel) {
      refuseOwner(responsePort, binding.reservationId);
      failBinding(binding, 'internal_error', false);
      return;
    }
    if (!binding.active || !currentBindingState(binding, 'waiting_for_owner')) {
      closeChannel(channel);
      refuseOwner(responsePort, binding.reservationId);
      if (binding.active) failBinding(binding, 'internal_error', false);
      return;
    }
    binding.controlPort = channel.retained;
    const registered = (() => {
      try {
        return (
          Reflect.apply(binding.attempt.ownerRegistered, binding.attempt, []) === true &&
          currentBindingState(binding, 'waiting_for_insertion')
        );
      } catch {
        return false;
      }
    })();
    const response = registered ? ownerResponse(binding.reservationId, ticket) : undefined;
    let posted = false;
    if (response) {
      try {
        posted = responsePort.post(response, [channel.transferred]) === true;
      } catch {
        posted = false;
      }
    }
    closePort(responsePort);
    closePort(channel.transferred);
    if (!registered || !posted || !binding.active) {
      if (binding.active) failBinding(binding, 'internal_error', false);
      return;
    }
    let sourceType: unknown;
    try {
      sourceType = binding.attempt.renderSource?.type;
    } catch {
      sourceType = undefined;
    }
    if (sourceType === 'adm') {
      startAdmOwner(binding, ticket);
    } else if (sourceType === 'aps') {
      startApsOwner(binding, ticket);
    } else {
      failBinding(binding, 'winner_not_renderable', false);
    }
  };

  const dispatch = (event: MessageEvent): void => {
    if (disposed) return;
    const data = eventData(event);
    const routing = messaging.inspectGlobalMessage(data);
    if (routing?.message === TSJS_MESSAGE_PROTOCOL_V1.message.ownerRegister) {
      handleOwnerRegistration(event, data, routing);
      return;
    }
    if (routing?.message !== TSJS_MESSAGE_PROTOCOL_V1.message.prebidRequest || !routing.adId) {
      return;
    }

    const recognition = recognizedReservation(reservations, routing.adId);
    if (recognition?.recognized !== true) return;
    if (!suppress(event)) return;

    const exact = messaging.parseProtocolMessage('prebidRequest', data);
    const inspection = messaging.inspectTransferredPorts(event);
    const ports = inspection?.ports;
    const port = ports?.[0];
    if (!inspection || !port) return;
    if (
      inspection.exactShape !== true ||
      inspection.originalCount !== 1 ||
      ports.length !== 1 ||
      exact === undefined ||
      recognition.state !== 'renderable'
    ) {
      refuse(port, routing.adId);
      for (let index = 1; index < ports.length; index += 1) {
        const extra = ports[index];
        if (extra) closePort(extra);
      }
      return;
    }

    const source = eventSource(event);
    const binding = mapValue(attempts, routing.adId);
    if (
      source === undefined ||
      !binding?.active ||
      binding.claim !== undefined ||
      !currentBindingState(binding, 'waiting_for_gam_and_claim')
    ) {
      refuse(port, routing.adId);
      return;
    }

    binding.claim = frozen({ port, source });
    binding.pucSource = source;
    if (binding.gamReady) join(binding);
  };

  const uninstall = messaging.installCaptureListener(dispatch);
  if (typeof uninstall !== 'function') {
    throw new Error('Universal Creative capture listener installation failed');
  }

  const bridge: PucBridge = {
    registerGamAttempt(input): boolean {
      if (disposed || !dynamicOwnerValid) return false;
      const exact = exactInput(input);
      if (!exact || mapValue(attempts, exact.reservationId) !== undefined) return false;
      const existing = snapshotMapValues(attempts);
      for (let index = 0; index < existing.length; index += 1) {
        const candidate = existing[index];
        if (
          candidate &&
          (candidate.attempt === exact.attempt ||
            candidate.attempt.generation === exact.attempt.generation)
        ) {
          return false;
        }
      }
      const recognition = recognizedReservation(reservations, exact.reservationId);
      if (recognition?.recognized !== true || recognition.state !== 'renderable') return false;
      try {
        const snapshot = Reflect.apply(exact.attempt.snapshot, exact.attempt, []);
        if (
          Reflect.apply(exact.owner.isCurrent, exact.owner, []) !== true ||
          snapshot.outcome !== undefined ||
          snapshot.state !== 'created' ||
          exact.attempt.renderSource !== undefined
        ) {
          return false;
        }
      } catch {
        return false;
      }
      const started = (() => {
        try {
          return Reflect.apply(exact.attempt.beginGamClaim, exact.attempt, []) === true;
        } catch {
          return false;
        }
      })();
      if (!started) return false;
      const binding: GamAttemptBinding = {
        reservationId: exact.reservationId,
        attempt: exact.attempt,
        artifact: exact.artifact,
        owner: exact.owner,
        attemptId: exact.attempt.id,
        slot: exact.owner.slot,
        navigationGeneration: exact.owner.navigationGeneration,
        artifactOwned: true,
        active: true,
        claim: undefined,
        claimDeadlineHandle: undefined,
        claimDeadlineToken: undefined,
        controlListenerDispose: undefined,
        controlPort: undefined,
        controlStarted: false,
        documentAccepted: false,
        documentAcceptancePending: false,
        documentTerminalPending: undefined,
        documentListenerDispose: undefined,
        documentPort: undefined,
        documentPortRegistryOwned: false,
        documentTransferredPort: undefined,
        gamReady: false,
        joining: false,
        lifecycleTicket: undefined,
        nonce: undefined,
        ownerInserted: false,
        pucSource: undefined,
        ticket: undefined,
      };
      setMapValue(attempts, exact.reservationId, binding);
      if (!currentBindingState(binding, 'waiting_for_gam_and_claim')) {
        cleanupBinding(binding);
        return false;
      }
      const observed = (() => {
        try {
          return (
            Reflect.apply(exact.attempt.onSettled, exact.attempt, [
              (outcome: RenderOutcome) => settleBinding(binding, outcome),
            ]) === true
          );
        } catch {
          return false;
        }
      })();
      if (!observed || !binding.active || mapValue(attempts, exact.reservationId) !== binding) {
        cleanupBinding(binding);
        try {
          Reflect.apply(exact.attempt.fail, exact.attempt, ['internal_error']);
        } catch {
          // Registration rejection already retains no bridge authority.
        }
        return false;
      }
      return true;
    },
    recordNonemptyGam(input): boolean {
      if (disposed) return false;
      const exact = exactInput(input);
      if (!exact) return false;
      const binding = mapValue(attempts, exact.reservationId);
      if (
        !binding ||
        !bindingMatches(binding, exact) ||
        binding.gamReady ||
        !currentBindingState(binding, 'waiting_for_gam_and_claim')
      ) {
        return false;
      }
      binding.gamReady = true;
      if (binding.claim) join(binding);
      else armClaimDeadline(binding);
      return true;
    },
    dispose(): void {
      if (disposed) return;
      disposed = true;
      try {
        uninstall();
      } catch {
        // Listener removal is already contained by the adapter.
      }
      const bindings = snapshotMapValues(attempts);
      for (let index = 0; index < bindings.length; index += 1) {
        const binding = bindings[index];
        if (!binding) continue;
        const cancelled = (() => {
          try {
            return (
              Reflect.apply(binding.attempt.cancel, binding.attempt, ['navigation_disposed']) ===
              true
            );
          } catch {
            return false;
          }
        })();
        if (!cancelled && binding.active) cleanupBinding(binding);
      }
      const ticketEntries = snapshotMapValues(tickets);
      for (let index = 0; index < ticketEntries.length; index += 1) {
        const entry = ticketEntries[index];
        if (entry?.state !== 'pending') clearScheduled(entry?.expiryHandle);
      }
      Reflect.apply(mapClearIntrinsic, attempts, []);
      Reflect.apply(mapClearIntrinsic, tickets, []);
    },
    snapshotInventoryForTest(): PucBridgeInventory {
      let pendingClaims = 0;
      const bindings = snapshotMapValues(attempts);
      for (let index = 0; index < bindings.length; index += 1) {
        if (bindings[index]?.claim) pendingClaims += 1;
      }
      let liveTickets = 0;
      let ticketTombstones = 0;
      const ticketEntries = snapshotMapValues(tickets);
      for (let index = 0; index < ticketEntries.length; index += 1) {
        if (ticketEntries[index]?.state === 'live' || ticketEntries[index]?.state === 'pending') {
          liveTickets += 1;
        } else if (ticketEntries[index]?.state === 'tombstone') ticketTombstones += 1;
      }
      return frozen({
        attempts: mapSize(attempts),
        disposed,
        liveTickets,
        pendingClaims,
        ticketTombstones,
      });
    },
  };
  return frozen(bridge);
}
