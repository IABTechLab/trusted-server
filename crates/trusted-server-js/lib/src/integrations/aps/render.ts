import type { ApsRendererV1 } from '../../core/types';
import { validateApsRenderer } from '../../core/contracts/aps_renderer';
import type { MessagingAdapter, MessagingChannel } from '../../adapters/messaging';
import type {
  CommittedRenderArtifact,
  RenderAttempt,
  RenderFailureReason,
  RendererNonceRegistry,
} from '../../services/render';

const objectFreezeIntrinsic = Object.freeze;
const objectGetPrototypeOfIntrinsic = Object.getPrototypeOf;
const regexpTestIntrinsic = RegExp.prototype.test;
const rendererNoncePattern = /^n1_[A-Za-z0-9_-]{22}$/;
const loopbackIpv4Pattern = /^127(?:\.\d{1,3}){3}$/;
const iframeNamespace = 'http://www.w3.org/1999/xhtml';
const directDomAvailable =
  typeof document !== 'undefined' &&
  typeof HTMLIFrameElement !== 'undefined' &&
  typeof Document !== 'undefined' &&
  typeof Node !== 'undefined' &&
  typeof Element !== 'undefined' &&
  typeof EventTarget !== 'undefined' &&
  typeof HTMLCollection !== 'undefined';
const directRenderDocument = directDomAvailable ? document : undefined;
const directIframePrototype = directDomAvailable ? HTMLIFrameElement.prototype : undefined;
const documentCreateElementIntrinsic = directDomAvailable
  ? Document.prototype.createElement
  : undefined;
const nodeAppendChildIntrinsic = directDomAvailable ? Node.prototype.appendChild : undefined;
const nodeRemoveChildIntrinsic = directDomAvailable ? Node.prototype.removeChild : undefined;
const elementRemoveIntrinsic = directDomAvailable ? Element.prototype.remove : undefined;
const elementSetAttributeIntrinsic = directDomAvailable
  ? Element.prototype.setAttribute
  : undefined;
const elementGetAttributeIntrinsic = directDomAvailable
  ? Element.prototype.getAttribute
  : undefined;
const eventTargetAddListenerIntrinsic = directDomAvailable
  ? EventTarget.prototype.addEventListener
  : undefined;
const eventTargetRemoveListenerIntrinsic = directDomAvailable
  ? EventTarget.prototype.removeEventListener
  : undefined;
const htmlCollectionItemIntrinsic = directDomAvailable ? HTMLCollection.prototype.item : undefined;
const nodeOwnerDocumentGetter = directDomAvailable
  ? Object.getOwnPropertyDescriptor(Node.prototype, 'ownerDocument')?.get
  : undefined;
const nodeParentNodeGetter = directDomAvailable
  ? Object.getOwnPropertyDescriptor(Node.prototype, 'parentNode')?.get
  : undefined;
const nodeIsConnectedGetter = directDomAvailable
  ? Object.getOwnPropertyDescriptor(Node.prototype, 'isConnected')?.get
  : undefined;
const elementLocalNameGetter = directDomAvailable
  ? Object.getOwnPropertyDescriptor(Element.prototype, 'localName')?.get
  : undefined;
const elementNamespaceGetter = directDomAvailable
  ? Object.getOwnPropertyDescriptor(Element.prototype, 'namespaceURI')?.get
  : undefined;
const elementChildrenGetter = directDomAvailable
  ? Object.getOwnPropertyDescriptor(Element.prototype, 'children')?.get
  : undefined;
const htmlCollectionLengthGetter = directDomAvailable
  ? Object.getOwnPropertyDescriptor(HTMLCollection.prototype, 'length')?.get
  : undefined;
const iframeContentWindowGetter = directDomAvailable
  ? Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'contentWindow')?.get
  : undefined;
const iframeSourceGetter = directDomAvailable
  ? Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'src')?.get
  : undefined;

export { parseApsRendererDescriptor, validateApsRenderer } from '../../core/contracts/aps_renderer';
export { APS_PERMANENT_SANDBOX, generateApsDataDocumentsV1 } from './documents';

export const APS_RENDERER_V1_PATH = '/integrations/aps/renderer/v1';
export const APS_RENDERER_SANDBOX =
  'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation';

/** Validate, copy, and freeze one APS tagged render source. */
export function prepareApsRenderSource(
  input: unknown,
  publisherOrigin?: string
): Readonly<ApsRendererV1> | undefined {
  try {
    const renderer = validateApsRenderer(input, publisherOrigin);
    return renderer
      ? (Reflect.apply(objectFreezeIntrinsic, Object, [renderer]) as Readonly<ApsRendererV1>)
      : undefined;
  } catch {
    return undefined;
  }
}

export interface DirectApsAttemptOptions {
  readonly attempt: RenderAttempt;
  readonly container: HTMLElement;
  readonly messaging: MessagingAdapter;
  readonly nonces: RendererNonceRegistry;
  readonly publisherOrigin: string;
}

function freeze<Value extends object>(value: Value): Readonly<Value> {
  return Reflect.apply(objectFreezeIntrinsic, Object, [value]) as Readonly<Value>;
}

export function resolveApsRendererV1Url(publisherOrigin: string): string | undefined {
  try {
    const origin = new URL(publisherOrigin);
    const loopbackHttp =
      origin.protocol === 'http:' &&
      (origin.hostname === 'localhost' ||
        origin.hostname === '[::1]' ||
        (Reflect.apply(regexpTestIntrinsic, loopbackIpv4Pattern, [origin.hostname]) as boolean));
    if (
      origin.origin !== publisherOrigin ||
      (origin.protocol !== 'https:' && !loopbackHttp) ||
      origin.username !== '' ||
      origin.password !== ''
    ) {
      return undefined;
    }
    const rendererUrl = new URL(APS_RENDERER_V1_PATH, origin);
    if (
      rendererUrl.origin !== origin.origin ||
      rendererUrl.pathname !== APS_RENDERER_V1_PATH ||
      rendererUrl.search !== '' ||
      rendererUrl.hash !== ''
    ) {
      return undefined;
    }
    return rendererUrl.href;
  } catch {
    return undefined;
  }
}

function closeChannel(channel: MessagingChannel | undefined): void {
  try {
    channel?.transferred.close();
  } catch {
    // The second endpoint must still be attempted when the first close is hostile.
  }
  try {
    channel?.retained.close();
  } catch {
    // Failed construction cleanup remains best-effort.
  }
}

function mapNonceIssueFailure(
  reason: 'capability_registry_full' | 'identity_generation_failed' | 'invalid_attempt'
): RenderFailureReason {
  return reason === 'invalid_attempt' ? 'internal_error' : reason;
}

function mapRunnerFailure(reason: unknown): RenderFailureReason | undefined {
  if (reason === 'descriptor_invalid') return 'winner_not_renderable';
  if (reason === 'runner_no_load' || reason === 'runner_failed') return reason;
  return undefined;
}

function readNonceIssueResult(value: unknown):
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
    const names = Object.getOwnPropertyNames(value);
    if (names.length !== 2) return undefined;
    const ok = Object.getOwnPropertyDescriptor(value, 'ok');
    if (!ok || !ok.enumerable || !('value' in ok)) return undefined;
    if (ok.value === true) {
      const nonce = Object.getOwnPropertyDescriptor(value, 'nonce');
      if (
        !nonce ||
        !nonce.enumerable ||
        !('value' in nonce) ||
        typeof nonce.value !== 'string' ||
        !(Reflect.apply(regexpTestIntrinsic, rendererNoncePattern, [nonce.value]) as boolean)
      ) {
        return undefined;
      }
      return freeze({ ok: true as const, nonce: nonce.value });
    }
    if (ok.value !== false) return undefined;
    const reason = Object.getOwnPropertyDescriptor(value, 'reason');
    if (
      !reason ||
      !reason.enumerable ||
      !('value' in reason) ||
      (reason.value !== 'capability_registry_full' &&
        reason.value !== 'identity_generation_failed' &&
        reason.value !== 'invalid_attempt')
    ) {
      return undefined;
    }
    return freeze({ ok: false as const, reason: reason.value });
  } catch {
    return undefined;
  }
}

/** Drive one direct APS attempt through the versioned static renderer document. */
export function renderDirectApsAttempt(options: DirectApsAttemptOptions): boolean {
  if (
    !directRenderDocument ||
    !directIframePrototype ||
    typeof documentCreateElementIntrinsic !== 'function' ||
    typeof nodeAppendChildIntrinsic !== 'function' ||
    typeof nodeRemoveChildIntrinsic !== 'function' ||
    typeof elementRemoveIntrinsic !== 'function' ||
    typeof elementSetAttributeIntrinsic !== 'function' ||
    typeof elementGetAttributeIntrinsic !== 'function' ||
    typeof eventTargetAddListenerIntrinsic !== 'function' ||
    typeof eventTargetRemoveListenerIntrinsic !== 'function' ||
    typeof htmlCollectionItemIntrinsic !== 'function' ||
    typeof nodeOwnerDocumentGetter !== 'function' ||
    typeof nodeParentNodeGetter !== 'function' ||
    typeof nodeIsConnectedGetter !== 'function' ||
    typeof elementLocalNameGetter !== 'function' ||
    typeof elementNamespaceGetter !== 'function' ||
    typeof elementChildrenGetter !== 'function' ||
    typeof htmlCollectionLengthGetter !== 'function' ||
    typeof iframeContentWindowGetter !== 'function' ||
    typeof iframeSourceGetter !== 'function'
  ) {
    return false;
  }
  let attempt: RenderAttempt;
  let messaging: MessagingAdapter;
  let nonces: RendererNonceRegistry;
  let container: HTMLElement;
  let publisherOrigin: string;
  let sourceCandidate: unknown;
  let attemptId: string;
  let attemptSlot: string;
  let attemptGeneration: object;
  let navigationGeneration: object;
  let ownerDocument: Document;
  try {
    attempt = options.attempt;
    messaging = options.messaging;
    nonces = options.nonces;
    container = options.container;
    publisherOrigin = options.publisherOrigin;
    sourceCandidate = attempt.renderSource;
    attemptId = attempt.id;
    attemptSlot = attempt.slot;
    attemptGeneration = attempt.generation;
    navigationGeneration = attempt.navigationGeneration;
    if (typeof nodeOwnerDocumentGetter !== 'function') return false;
    ownerDocument = Reflect.apply(nodeOwnerDocumentGetter, container, []) as Document;
  } catch {
    return false;
  }
  let exactDocumentOrigin: boolean;
  try {
    exactDocumentOrigin =
      ownerDocument === directRenderDocument &&
      ownerDocument.defaultView?.location.origin === publisherOrigin;
  } catch {
    exactDocumentOrigin = false;
  }
  const renderer = prepareApsRenderSource(sourceCandidate, publisherOrigin);
  const rendererUrl = resolveApsRendererV1Url(publisherOrigin);
  if (!exactDocumentOrigin || !renderer || !rendererUrl) {
    try {
      attempt.fail('winner_not_renderable');
    } catch {
      // Invalid input remains rejected even when the attempt boundary is hostile.
    }
    return false;
  }
  let createChannelMethod: MessagingAdapter['createChannel'];
  let postWindowMethod: MessagingAdapter['postWindow'];
  let parseMessageMethod: MessagingAdapter['parseProtocolMessage'];
  let issueMethod: RendererNonceRegistry['issue'];
  let bindSourceMethod: RendererNonceRegistry['bindSource'];
  let consumeMethod: RendererNonceRegistry['consume'];
  let beginDirectMethod: RenderAttempt['beginDirect'];
  let beginDocumentMethod: RenderAttempt['beginApsDocument'];
  let documentAcceptedMethod: RenderAttempt['apsDocumentAccepted'];
  let acceptMethod: RenderAttempt['accept'];
  let failMethod: RenderAttempt['fail'];
  let snapshotMethod: RenderAttempt['snapshot'];
  try {
    createChannelMethod = messaging.createChannel;
    postWindowMethod = messaging.postWindow;
    parseMessageMethod = messaging.parseProtocolMessage;
    issueMethod = nonces.issue;
    bindSourceMethod = nonces.bindSource;
    consumeMethod = nonces.consume;
    beginDirectMethod = attempt.beginDirect;
    beginDocumentMethod = attempt.beginApsDocument;
    documentAcceptedMethod = attempt.apsDocumentAccepted;
    acceptMethod = attempt.accept;
    failMethod = attempt.fail;
    snapshotMethod = attempt.snapshot;
    if (
      typeof createChannelMethod !== 'function' ||
      typeof postWindowMethod !== 'function' ||
      typeof parseMessageMethod !== 'function' ||
      typeof issueMethod !== 'function' ||
      typeof bindSourceMethod !== 'function' ||
      typeof consumeMethod !== 'function' ||
      typeof beginDirectMethod !== 'function' ||
      typeof beginDocumentMethod !== 'function' ||
      typeof documentAcceptedMethod !== 'function' ||
      typeof acceptMethod !== 'function' ||
      typeof failMethod !== 'function' ||
      typeof snapshotMethod !== 'function' ||
      Reflect.apply(beginDirectMethod, attempt, []) !== true
    ) {
      return false;
    }
  } catch {
    return false;
  }

  const fail = (reason: RenderFailureReason): false => {
    try {
      Reflect.apply(failMethod, attempt, [reason]);
    } catch {
      // The attempt's terminal latch owns failure authority.
    }
    return false;
  };

  const attemptState = (): ReturnType<RenderAttempt['snapshot']>['state'] | undefined => {
    try {
      return Reflect.apply(snapshotMethod, attempt, []).state;
    } catch {
      return undefined;
    }
  };

  let iframe: HTMLIFrameElement;
  try {
    if (
      typeof nodeParentNodeGetter !== 'function' ||
      typeof nodeIsConnectedGetter !== 'function' ||
      typeof elementLocalNameGetter !== 'function' ||
      typeof elementNamespaceGetter !== 'function' ||
      typeof elementChildrenGetter !== 'function' ||
      typeof htmlCollectionLengthGetter !== 'function' ||
      typeof iframeContentWindowGetter !== 'function' ||
      typeof iframeSourceGetter !== 'function'
    ) {
      return fail('renderer_document_no_load');
    }
    iframe = Reflect.apply(documentCreateElementIntrinsic, ownerDocument, [
      'iframe',
    ]) as HTMLIFrameElement;
    if (
      typeof iframe !== 'object' ||
      iframe === null ||
      Reflect.apply(objectGetPrototypeOfIntrinsic, Object, [iframe]) !== directIframePrototype ||
      Reflect.apply(nodeOwnerDocumentGetter, iframe, []) !== ownerDocument ||
      Reflect.apply(elementLocalNameGetter, iframe, []) !== 'iframe' ||
      Reflect.apply(elementNamespaceGetter, iframe, []) !== iframeNamespace ||
      Reflect.apply(nodeParentNodeGetter, iframe, []) !== null ||
      Reflect.apply(nodeIsConnectedGetter, iframe, []) === true
    ) {
      return fail('renderer_document_no_load');
    }
    const attributes = [
      ['title', 'Ad content'],
      ['scrolling', 'no'],
      ['frameborder', '0'],
      ['width', String(renderer.width)],
      ['height', String(renderer.height)],
      ['aria-label', 'Advertisement'],
      ['marginheight', '0'],
      ['marginwidth', '0'],
      ['sandbox', APS_RENDERER_SANDBOX],
      [
        'style',
        `border: 0; display: block; height: ${renderer.height}px; margin: 0; overflow: hidden; width: ${renderer.width}px`,
      ],
    ] as const;
    for (let index = 0; index < attributes.length; index += 1) {
      const attribute = attributes[index];
      if (attribute) {
        Reflect.apply(elementSetAttributeIntrinsic, iframe, [attribute[0], attribute[1]]);
      }
    }
  } catch {
    return fail('renderer_document_no_load');
  }

  let channel: MessagingChannel | undefined;
  try {
    channel = Reflect.apply(createChannelMethod, messaging, []);
  } catch {
    channel = undefined;
  }
  if (!channel) return fail('internal_error');
  let issueResult: unknown;
  try {
    issueResult = Reflect.apply(issueMethod, nonces, [{ attempt, port: channel.retained }]);
  } catch {
    closeChannel(channel);
    return fail('identity_generation_failed');
  }
  const issued = readNonceIssueResult(issueResult);
  if (!issued) {
    closeChannel(channel);
    return fail('identity_generation_failed');
  }
  if (!issued.ok) {
    closeChannel(channel);
    return fail(mapNonceIssueFailure(issued.reason));
  }
  const nonce = issued.nonce;
  let boundSource: object | undefined;
  let documentAccepted = false;
  let disposed = false;
  let sourceAssigned = false;
  let appendInProgress = false;
  let insertionCommitted = false;
  let loadObserved = false;
  let errorObserved = false;
  let envelopeTransferred = false;
  let artifactOwnedByAttempt = false;
  let insertionPredecessors: readonly Element[] = [];
  const expectedFrameSource = `${rendererUrl}#tsaps=${nonce}`;

  const removeFrameListeners = (): void => {
    try {
      Reflect.apply(eventTargetRemoveListenerIntrinsic, iframe, ['load', onLoad]);
    } catch {
      // Listener removal cannot interrupt terminal resource cleanup.
    }
    try {
      Reflect.apply(eventTargetRemoveListenerIntrinsic, iframe, ['error', onError]);
    } catch {
      // The second listener is always attempted.
    }
  };

  const artifact: CommittedRenderArtifact = freeze({
    kind: 'direct_iframe' as const,
    attemptId,
    slot: attemptSlot,
    navigationGeneration,
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      removeFrameListeners();
      try {
        channel?.transferred.close();
      } catch {
        // A transferred endpoint is inert; an untransferred endpoint is locally closed.
      }
      try {
        Reflect.apply(elementRemoveIntrinsic, iframe, []);
      } catch {
        // DOM removal remains best-effort under a hostile page.
      }
    },
  });

  const startupFailure = (reason: RenderFailureReason): false => {
    if (!artifactOwnedByAttempt) artifact.dispose();
    return fail(reason);
  };

  const nonceExpectation = (source: object) =>
    freeze({ nonce, attempt, generation: attemptGeneration, source, port: channel!.retained });

  const messageData = (event: unknown): unknown => {
    try {
      return typeof event === 'object' && event !== null ? Reflect.get(event, 'data') : undefined;
    } catch {
      return undefined;
    }
  };

  const snapshotContainerPredecessors = (): readonly Element[] => {
    const predecessors: Element[] = [];
    try {
      const children = Reflect.apply(elementChildrenGetter!, container, []) as HTMLCollection;
      const length = Reflect.apply(htmlCollectionLengthGetter!, children, []) as number;
      for (let index = 0; index < length; index += 1) {
        const child = Reflect.apply(htmlCollectionItemIntrinsic, children, [
          index,
        ]) as Element | null;
        if (child && child !== iframe) predecessors[predecessors.length] = child;
      }
    } catch {
      // Failure to inspect publisher siblings cannot expand cleanup authority.
    }
    return predecessors;
  };

  const commitContainer = (predecessors: readonly Element[]): void => {
    try {
      for (let index = predecessors.length - 1; index >= 0; index -= 1) {
        const child = predecessors[index];
        if (
          child &&
          child !== iframe &&
          Reflect.apply(nodeParentNodeGetter!, child, []) === container
        ) {
          Reflect.apply(nodeRemoveChildIntrinsic, container, [child]);
        }
      }
    } catch {
      // The accepted artifact remains authoritative if publisher sibling cleanup is hostile.
    }
  };

  const exactFrameBinding = (): boolean => {
    try {
      // This binds the native element and browsing context. An opaque Document cannot
      // be attested after ancestor-controlled contentWindow.location navigation (§4.4).
      return (
        insertionCommitted &&
        Reflect.apply(nodeParentNodeGetter!, iframe, []) === container &&
        Reflect.apply(nodeIsConnectedGetter!, iframe, []) === true &&
        Reflect.apply(iframeContentWindowGetter!, iframe, []) === boundSource &&
        Reflect.apply(elementGetAttributeIntrinsic, iframe, ['src']) === expectedFrameSource &&
        Reflect.apply(iframeSourceGetter!, iframe, []) === expectedFrameSource
      );
    } catch {
      return false;
    }
  };

  const receive = (event: unknown): void => {
    if (disposed || !boundSource || !envelopeTransferred) return;
    if (!exactFrameBinding()) {
      fail(documentAccepted ? 'runner_failed' : 'renderer_document_no_load');
      return;
    }
    const data = messageData(event);
    const accepted = Reflect.apply(parseMessageMethod, messaging, ['apsDocumentAccepted', data]);
    if (accepted?.['nonce'] === nonce) {
      if (documentAccepted || attemptState() !== 'waiting_for_document') return;
      const expectation = nonceExpectation(boundSource);
      if (
        Reflect.apply(consumeMethod, nonces, [expectation]) === true &&
        Reflect.apply(documentAcceptedMethod, attempt, []) === true
      ) {
        documentAccepted = true;
      }
      return;
    }
    const loaded = Reflect.apply(parseMessageMethod, messaging, ['apsRunnerLoaded', data]);
    if (loaded?.['nonce'] === nonce) return;
    const completed = Reflect.apply(parseMessageMethod, messaging, ['apsRenderCompleted', data]);
    if (completed?.['nonce'] === nonce) {
      if (documentAccepted && Reflect.apply(acceptMethod, attempt, []) === true) {
        if (!disposed && exactFrameBinding()) commitContainer(insertionPredecessors);
      }
      return;
    }
    const failed = Reflect.apply(parseMessageMethod, messaging, ['apsRenderFailed', data]);
    if (failed?.['nonce'] !== nonce) return;
    const reason = mapRunnerFailure(failed['reason']);
    if (reason) Reflect.apply(failMethod, attempt, [reason]);
  };

  const receiveError = (): void => {
    if (disposed || !envelopeTransferred) return;
    fail(documentAccepted ? 'runner_failed' : 'renderer_document_no_load');
  };

  const transferEnvelope = (): void => {
    if (disposed || envelopeTransferred || !loadObserved || !boundSource) return;
    if (!exactFrameBinding()) {
      fail('renderer_document_no_load');
      return;
    }
    if (attemptState() !== 'waiting_for_document') {
      fail('internal_error');
      return;
    }
    const envelope = freeze({ version: 1 as const, nonce, publisherOrigin, renderer });
    const posted = Reflect.apply(postWindowMethod, messaging, [
      boundSource,
      envelope,
      '*',
      [channel!.transferred],
    ]);
    if (posted !== true || !exactFrameBinding()) {
      fail('renderer_document_no_load');
      return;
    }
    envelopeTransferred = true;
    removeFrameListeners();
  };

  function onLoad(): void {
    if (
      disposed ||
      !sourceAssigned ||
      (!insertionCommitted &&
        !(appendInProgress && Reflect.apply(nodeParentNodeGetter!, iframe, []) === container))
    ) {
      return;
    }
    loadObserved = true;
    transferEnvelope();
  }

  function onError(): void {
    if (
      disposed ||
      envelopeTransferred ||
      !sourceAssigned ||
      (!insertionCommitted &&
        !(appendInProgress && Reflect.apply(nodeParentNodeGetter!, iframe, []) === container))
    ) {
      return;
    }
    errorObserved = true;
    if (artifactOwnedByAttempt) fail('renderer_document_no_load');
  }

  try {
    channel.retained.listen(receive, receiveError);
    Reflect.apply(eventTargetAddListenerIntrinsic, iframe, ['load', onLoad, { once: true }]);
    Reflect.apply(eventTargetAddListenerIntrinsic, iframe, ['error', onError, { once: true }]);
    Reflect.apply(elementSetAttributeIntrinsic, iframe, ['src', expectedFrameSource]);
    if (
      Reflect.apply(elementGetAttributeIntrinsic, iframe, ['src']) !== expectedFrameSource ||
      Reflect.apply(iframeSourceGetter!, iframe, []) !== expectedFrameSource
    ) {
      return startupFailure('renderer_document_no_load');
    }
    sourceAssigned = true;
    if (
      disposed ||
      attemptState() !== 'rendering_direct' ||
      Reflect.apply(nodeIsConnectedGetter, container, []) !== true
    ) {
      return startupFailure('renderer_document_no_load');
    }
    insertionPredecessors = snapshotContainerPredecessors();
    if (
      disposed ||
      attemptState() !== 'rendering_direct' ||
      Reflect.apply(nodeIsConnectedGetter, container, []) !== true
    ) {
      return startupFailure('renderer_document_no_load');
    }
    appendInProgress = true;
    try {
      Reflect.apply(nodeAppendChildIntrinsic, container, [iframe]);
    } finally {
      appendInProgress = false;
    }
    if (Reflect.apply(nodeParentNodeGetter!, iframe, []) !== container) {
      return startupFailure('renderer_document_no_load');
    }
    insertionCommitted = true;
    if (Reflect.apply(beginDocumentMethod, attempt, [artifact]) !== true) {
      return startupFailure('internal_error');
    }
    artifactOwnedByAttempt = true;
    if (disposed) return false;
    if (errorObserved) return fail('renderer_document_no_load');
    if (attemptState() !== 'waiting_for_document') return fail('internal_error');
    const source = Reflect.apply(iframeContentWindowGetter!, iframe, []) as Window | null;
    if (!source) return fail('renderer_document_no_load');
    boundSource = source;
    if (!exactFrameBinding()) return fail('renderer_document_no_load');
    if (Reflect.apply(bindSourceMethod, nonces, [nonceExpectation(source)]) !== true) {
      return fail('renderer_document_no_load');
    }
    transferEnvelope();
    return true;
  } catch {
    return startupFailure('renderer_document_no_load');
  }
}
