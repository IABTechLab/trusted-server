import type { ApsRendererV1 } from '../../core/types';
import { validateApsRenderer } from '../../core/contracts/aps_renderer';
import type { MessagingAdapter, MessagingPort } from '../../adapters/messaging';
import type {
  ArtifactHostPositionLeaseRegistry,
  BootstrapNonceRegistry,
  CommittedRenderArtifact,
  RenderAttempt,
  RenderFailureReason,
  RendererNonceRegistry,
} from '../../services/render';
import type { ApsSlotMountBinding } from '../../services/slots';

const objectFreezeIntrinsic = Object.freeze;
const objectGetPrototypeOfIntrinsic = Object.getPrototypeOf;
const regexpTestIntrinsic = RegExp.prototype.test;
const bootstrapNoncePattern = /^b1_[A-Za-z0-9_-]{22}$/;
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
const documentGetElementByIdIntrinsic = directDomAvailable
  ? Document.prototype.getElementById
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
const elementIdGetter = directDomAvailable
  ? Object.getOwnPropertyDescriptor(Element.prototype, 'id')?.get
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

export const APS_RENDERER_V2_PATH = '/integrations/aps/renderer/v2';
export const APS_RENDERER_SANDBOX =
  'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation';
export const APS_PERMANENT_SANDBOX =
  'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-same-origin allow-scripts allow-top-navigation-by-user-activation';

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
  readonly bindArtifactGuard: (
    artifact: CommittedRenderArtifact,
    current: () => boolean
  ) => boolean;
  readonly bootstrapNonces: BootstrapNonceRegistry;
  readonly container: HTMLElement;
  readonly messaging: MessagingAdapter;
  readonly nonces: RendererNonceRegistry;
  readonly publisherOrigin: string;
}

export interface PucApsAttemptOptions extends DirectApsAttemptOptions {
  readonly baseArtifact: CommittedRenderArtifact;
  readonly bindArtifact: ApsSlotMountBinding['bindArtifact'];
  readonly hostPositions: ArtifactHostPositionLeaseRegistry;
  readonly isBindingCurrent: () => boolean;
  readonly onArtifactTransferred: () => void;
}

type ApsTopPageAttemptOptions =
  | (DirectApsAttemptOptions & Readonly<{ mode: 'direct' }>)
  | (PucApsAttemptOptions & Readonly<{ mode: 'puc_overlay' }>);

function freeze<Value extends object>(value: Value): Readonly<Value> {
  return Reflect.apply(objectFreezeIntrinsic, Object, [value]) as Readonly<Value>;
}

export function resolveApsRendererV2Url(publisherOrigin: string): string | undefined {
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
    const rendererUrl = new URL(APS_RENDERER_V2_PATH, origin);
    if (
      rendererUrl.origin !== origin.origin ||
      rendererUrl.pathname !== APS_RENDERER_V2_PATH ||
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

function readNonceIssueResult(
  value: unknown,
  pattern: RegExp
):
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
        !(Reflect.apply(regexpTestIntrinsic, pattern, [nonce.value]) as boolean)
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

/** Drive one APS attempt through the shared three-phase top-page mount protocol. */
export function mountApsTopPageAttempt(options: ApsTopPageAttemptOptions): boolean {
  if (
    !directRenderDocument ||
    !directIframePrototype ||
    typeof documentCreateElementIntrinsic !== 'function' ||
    typeof documentGetElementByIdIntrinsic !== 'function' ||
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
    typeof elementIdGetter !== 'function' ||
    typeof htmlCollectionLengthGetter !== 'function' ||
    typeof iframeContentWindowGetter !== 'function' ||
    typeof iframeSourceGetter !== 'function'
  ) {
    return false;
  }

  let attempt: RenderAttempt;
  let bindArtifactGuard: DirectApsAttemptOptions['bindArtifactGuard'];
  let bootstrapNonces: BootstrapNonceRegistry;
  let rendererNonces: RendererNonceRegistry;
  let messaging: MessagingAdapter;
  let container: HTMLElement;
  let publisherOrigin: string;
  let sourceCandidate: unknown;
  let attemptId: string;
  let attemptSlot: string;
  let attemptGeneration: object;
  let navigationGeneration: object;
  let ownerDocument: Document;
  let mode: ApsTopPageAttemptOptions['mode'];
  let baseArtifact: CommittedRenderArtifact | undefined;
  let bindArtifact: ApsSlotMountBinding['bindArtifact'] | undefined;
  let hostPositions: ArtifactHostPositionLeaseRegistry | undefined;
  let isBindingCurrent: () => boolean;
  let onArtifactTransferred: () => void;
  let expectedContainerId: string | undefined;
  try {
    attempt = options.attempt;
    bindArtifactGuard = options.bindArtifactGuard;
    bootstrapNonces = options.bootstrapNonces;
    rendererNonces = options.nonces;
    messaging = options.messaging;
    container = options.container;
    publisherOrigin = options.publisherOrigin;
    sourceCandidate = attempt.renderSource;
    attemptId = attempt.id;
    attemptSlot = attempt.slot;
    attemptGeneration = attempt.generation;
    navigationGeneration = attempt.navigationGeneration;
    ownerDocument = Reflect.apply(nodeOwnerDocumentGetter, container, []) as Document;
    mode = options.mode;
    baseArtifact = options.mode === 'puc_overlay' ? options.baseArtifact : undefined;
    bindArtifact = options.mode === 'puc_overlay' ? options.bindArtifact : undefined;
    hostPositions = options.mode === 'puc_overlay' ? options.hostPositions : undefined;
    isBindingCurrent = options.mode === 'puc_overlay' ? options.isBindingCurrent : () => true;
    onArtifactTransferred =
      options.mode === 'puc_overlay' ? options.onArtifactTransferred : () => undefined;
    expectedContainerId =
      options.mode === 'puc_overlay'
        ? (Reflect.apply(elementIdGetter, container, []) as string)
        : undefined;
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
  const rendererUrl = resolveApsRendererV2Url(publisherOrigin);
  let creativeOrigin: string | undefined;
  try {
    creativeOrigin = renderer ? new URL(renderer.creativeUrl).origin : undefined;
  } catch {
    creativeOrigin = undefined;
  }
  if (
    !exactDocumentOrigin ||
    !renderer ||
    !rendererUrl ||
    !creativeOrigin ||
    (mode === 'puc_overlay' && !expectedContainerId)
  ) {
    try {
      attempt.fail('winner_not_renderable');
    } catch {
      // Invalid input remains rejected even when the attempt boundary is hostile.
    }
    return false;
  }

  let installCaptureMethod: MessagingAdapter['installCaptureListener'];
  let extractPortsMethod: MessagingAdapter['extractTransferredPorts'];
  let postWindowMethod: MessagingAdapter['postWindow'];
  let parseMessageMethod: MessagingAdapter['parseProtocolMessage'];
  let issueBootstrapMethod: BootstrapNonceRegistry['issue'];
  let bindBootstrapMethod: BootstrapNonceRegistry['bindSource'];
  let consumeBootstrapMethod: BootstrapNonceRegistry['consume'];
  let issueRendererMethod: RendererNonceRegistry['issue'];
  let bindRendererMethod: RendererNonceRegistry['bindSource'];
  let consumeRendererMethod: RendererNonceRegistry['consume'];
  let beginDirectMethod: RenderAttempt['beginDirect'];
  let beginDocumentMethod: RenderAttempt['beginApsDocument'];
  let documentAcceptedMethod: RenderAttempt['apsDocumentAccepted'];
  let acceptMethod: RenderAttempt['accept'];
  let failMethod: RenderAttempt['fail'];
  let snapshotMethod: RenderAttempt['snapshot'];
  try {
    installCaptureMethod = messaging.installCaptureListener;
    extractPortsMethod = messaging.extractTransferredPorts;
    postWindowMethod = messaging.postWindow;
    parseMessageMethod = messaging.parseProtocolMessage;
    issueBootstrapMethod = bootstrapNonces.issue;
    bindBootstrapMethod = bootstrapNonces.bindSource;
    consumeBootstrapMethod = bootstrapNonces.consume;
    issueRendererMethod = rendererNonces.issue;
    bindRendererMethod = rendererNonces.bindSource;
    consumeRendererMethod = rendererNonces.consume;
    beginDirectMethod = attempt.beginDirect;
    beginDocumentMethod = attempt.beginApsDocument;
    documentAcceptedMethod = attempt.apsDocumentAccepted;
    acceptMethod = attempt.accept;
    failMethod = attempt.fail;
    snapshotMethod = attempt.snapshot;
    if (
      typeof installCaptureMethod !== 'function' ||
      typeof extractPortsMethod !== 'function' ||
      typeof postWindowMethod !== 'function' ||
      typeof parseMessageMethod !== 'function' ||
      typeof issueBootstrapMethod !== 'function' ||
      typeof bindBootstrapMethod !== 'function' ||
      typeof consumeBootstrapMethod !== 'function' ||
      typeof issueRendererMethod !== 'function' ||
      typeof bindRendererMethod !== 'function' ||
      typeof consumeRendererMethod !== 'function' ||
      typeof beginDocumentMethod !== 'function' ||
      typeof documentAcceptedMethod !== 'function' ||
      typeof acceptMethod !== 'function' ||
      typeof failMethod !== 'function' ||
      typeof snapshotMethod !== 'function' ||
      (mode === 'direct'
        ? typeof beginDirectMethod !== 'function' ||
          Reflect.apply(beginDirectMethod, attempt, []) !== true
        : attempt.snapshot().state !== 'waiting_for_insertion' || !isBindingCurrent())
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
  const exactAttemptIdentity = (): boolean => {
    try {
      return (
        attempt.id === attemptId &&
        attempt.slot === attemptSlot &&
        attempt.generation === attemptGeneration &&
        attempt.navigationGeneration === navigationGeneration &&
        (mode === 'direct' || isBindingCurrent())
      );
    } catch {
      return false;
    }
  };

  let bootstrapIssue: unknown;
  try {
    bootstrapIssue = Reflect.apply(issueBootstrapMethod, bootstrapNonces, [{ attempt }]);
  } catch {
    return fail('identity_generation_failed');
  }
  const issuedBootstrap = readNonceIssueResult(bootstrapIssue, bootstrapNoncePattern);
  if (!issuedBootstrap) return fail('identity_generation_failed');
  if (!issuedBootstrap.ok) return fail(mapNonceIssueFailure(issuedBootstrap.reason));

  let rendererIssue: unknown;
  try {
    rendererIssue = Reflect.apply(issueRendererMethod, rendererNonces, [{ attempt }]);
  } catch {
    return fail('identity_generation_failed');
  }
  const issuedRenderer = readNonceIssueResult(rendererIssue, rendererNoncePattern);
  if (!issuedRenderer) return fail('identity_generation_failed');
  if (!issuedRenderer.ok) return fail(mapNonceIssueFailure(issuedRenderer.reason));

  const bootstrapNonce = issuedBootstrap.nonce;
  const rendererNonce = issuedRenderer.nonce;

  let iframe: HTMLIFrameElement;
  try {
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
        mode === 'puc_overlay'
          ? 'border: 0; display: block; height: ' +
            String(renderer.height) +
            'px; inset: 0; margin: 0; overflow: hidden; position: absolute; visibility: hidden; width: ' +
            String(renderer.width) +
            'px; z-index: 2147483647'
          : 'border: 0; display: block; height: ' +
            String(renderer.height) +
            'px; margin: 0; overflow: hidden; width: ' +
            String(renderer.width) +
            'px',
      ],
    ] as const;
    for (let attributeIndex = 0; attributeIndex < attributes.length; attributeIndex += 1) {
      const attribute = attributes[attributeIndex];
      if (attribute) {
        Reflect.apply(elementSetAttributeIntrinsic, iframe, [attribute[0], attribute[1]]);
      }
    }
  } catch {
    return fail('renderer_document_no_load');
  }

  let disposed = false;
  let artifactOwnedByAttempt = false;
  let insertionCommitted = false;
  let appendInProgress = false;
  let frameErrorObserved = false;
  let frameSource: object | undefined;
  let bootstrapBound = false;
  let navigationPosted = false;
  let containerReady = false;
  let envelopeSent = false;
  let documentAccepted = false;
  let documentPort: MessagingPort | undefined;
  let releaseGlobalListener: (() => void) | undefined;
  let insertionPredecessors: readonly Element[] = [];
  let baseArtifactDisposed = false;
  let baseArtifactTransferred = false;
  let hostPositionBound = false;
  const expectedFrameSource = rendererUrl + '#' + bootstrapNonce;

  const disposeBaseArtifact = (): void => {
    if (mode !== 'puc_overlay' || !baseArtifactTransferred || baseArtifactDisposed) return;
    baseArtifactDisposed = true;
    try {
      baseArtifact?.dispose();
    } catch {
      // The combined artifact remains terminal even when prior cleanup is hostile.
    }
  };

  const restoreHostPosition = (): void => {
    try {
      hostPositions?.release(artifact);
    } catch {
      // Compare-owned style restoration cannot expand into publisher styles.
    }
  };

  const bootstrapListenerResource = freeze({
    close: (): void => {
      const release = releaseGlobalListener;
      releaseGlobalListener = undefined;
      if (!release) return;
      try {
        release();
      } catch {
        // Capture-listener removal remains best-effort and exact-once.
      }
    },
  });

  const removeFrameErrorListener = (): void => {
    try {
      Reflect.apply(eventTargetRemoveListenerIntrinsic, iframe, ['error', onFrameError]);
    } catch {
      // Listener cleanup cannot interrupt terminal resource disposal.
    }
  };

  let artifactBinding:
    | Readonly<{
        commit: () => boolean;
        finalize: () => void;
        isCurrent: () => boolean;
        previousArtifact: CommittedRenderArtifact | undefined;
        release: () => void;
        rollback: () => void;
      }>
    | undefined;
  const artifact: CommittedRenderArtifact = freeze({
    kind: 'aps_mount' as const,
    attemptId,
    slot: attemptSlot,
    navigationGeneration,
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      removeFrameErrorListener();
      bootstrapListenerResource.close();
      try {
        documentPort?.close();
      } catch {
        // The renderer registry also owns exact-once retained-port cleanup.
      }
      try {
        Reflect.apply(elementRemoveIntrinsic, iframe, []);
      } catch {
        // DOM removal remains best-effort under a hostile publisher realm.
      }
      restoreHostPosition();
      artifactBinding?.release();
      disposeBaseArtifact();
    },
  });

  const persistentFrameCurrent = (): boolean => {
    try {
      return (
        insertionCommitted &&
        !disposed &&
        Reflect.apply(nodeOwnerDocumentGetter, iframe, []) === ownerDocument &&
        Reflect.apply(nodeParentNodeGetter, iframe, []) === container &&
        Reflect.apply(nodeIsConnectedGetter, iframe, []) === true &&
        (mode !== 'puc_overlay' ||
          (Reflect.apply(elementIdGetter, container, []) === expectedContainerId &&
            Reflect.apply(documentGetElementByIdIntrinsic, ownerDocument, [expectedContainerId]) ===
              container)) &&
        Reflect.apply(iframeContentWindowGetter, iframe, []) === frameSource &&
        Reflect.apply(elementGetAttributeIntrinsic, iframe, ['src']) === expectedFrameSource &&
        Reflect.apply(iframeSourceGetter, iframe, []) === expectedFrameSource &&
        Reflect.apply(elementGetAttributeIntrinsic, iframe, ['sandbox']) ===
          APS_PERMANENT_SANDBOX &&
        (mode !== 'puc_overlay' ||
          !hostPositionBound ||
          hostPositions?.current(artifact) === true) &&
        (mode !== 'puc_overlay' || artifactBinding?.isCurrent() === true)
      );
    } catch {
      return false;
    }
  };
  if (!bindArtifactGuard(artifact, persistentFrameCurrent)) {
    artifact.dispose();
    return fail('internal_error');
  }
  if (mode === 'puc_overlay') {
    try {
      artifactBinding = bindArtifact?.(artifact);
    } catch {
      artifactBinding = undefined;
    }
    if (!artifactBinding) {
      artifact.dispose();
      return fail('slot_unresolved');
    }
  }

  const startupFailure = (reason: RenderFailureReason): false => {
    if (!artifactOwnedByAttempt) artifact.dispose();
    return fail(reason);
  };

  const snapshotContainerPredecessors = (): readonly Element[] => {
    const predecessors: Element[] = [];
    try {
      const children = Reflect.apply(elementChildrenGetter, container, []) as HTMLCollection;
      const length = Reflect.apply(htmlCollectionLengthGetter, children, []) as number;
      for (let childIndex = 0; childIndex < length; childIndex += 1) {
        const child = Reflect.apply(htmlCollectionItemIntrinsic, children, [
          childIndex,
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
      for (let childIndex = predecessors.length - 1; childIndex >= 0; childIndex -= 1) {
        const child = predecessors[childIndex];
        if (
          child &&
          child !== iframe &&
          Reflect.apply(nodeParentNodeGetter, child, []) === container
        ) {
          Reflect.apply(nodeRemoveChildIntrinsic, container, [child]);
        }
      }
    } catch {
      // A hostile predecessor cannot revoke the accepted artifact.
    }
  };

  const acquireOverlayPosition = (): boolean => {
    if (mode !== 'puc_overlay') return true;
    try {
      if (!isBindingCurrent()) return false;
      const view = ownerDocument.defaultView;
      if (!view) return false;
      const computed = view.getComputedStyle(container);
      if (computed.position !== 'static') {
        const previousArtifact = artifactBinding?.previousArtifact;
        hostPositionBound = Boolean(
          previousArtifact && hostPositions?.inherit(artifact, previousArtifact, container)
        );
        return true;
      }
      const style = container.style;
      const previousPosition = style.getPropertyValue('position');
      const previousPriority = style.getPropertyPriority('position');
      style.setProperty('position', 'relative');
      if (
        style.getPropertyValue('position') !== 'relative' ||
        style.getPropertyPriority('position') !== ''
      ) {
        return false;
      }
      hostPositionBound =
        hostPositions?.bindOwned(artifact, container, previousPosition, previousPriority) === true;
      return hostPositionBound && isBindingCurrent();
    } catch {
      return false;
    }
  };

  const claimOverlayPositionLease = (): boolean => {
    if (mode !== 'puc_overlay' || !hostPositionBound) return true;
    try {
      return isBindingCurrent() && hostPositions?.claim(artifact) === true;
    } catch {
      return false;
    }
  };

  const exactFrameBinding = (permanent: boolean): boolean => {
    try {
      return (
        insertionCommitted &&
        !disposed &&
        exactAttemptIdentity() &&
        Reflect.apply(nodeOwnerDocumentGetter, iframe, []) === ownerDocument &&
        Reflect.apply(nodeParentNodeGetter, iframe, []) === container &&
        Reflect.apply(nodeIsConnectedGetter, iframe, []) === true &&
        Reflect.apply(iframeContentWindowGetter, iframe, []) === frameSource &&
        Reflect.apply(elementGetAttributeIntrinsic, iframe, ['src']) === expectedFrameSource &&
        Reflect.apply(iframeSourceGetter, iframe, []) === expectedFrameSource &&
        Reflect.apply(elementGetAttributeIntrinsic, iframe, ['sandbox']) ===
          (permanent ? APS_PERMANENT_SANDBOX : APS_RENDERER_SANDBOX)
      );
    } catch {
      return false;
    }
  };

  const eventField = (event: unknown, key: 'data' | 'origin' | 'source'): unknown => {
    try {
      return typeof event === 'object' && event !== null ? Reflect.get(event, key) : undefined;
    } catch {
      return undefined;
    }
  };

  const exactGlobalMessage = (
    kind: 'apsBootstrapReady' | 'apsContainerReady',
    data: unknown,
    expected: Readonly<Record<string, unknown>>
  ): boolean => {
    if (typeof data !== 'string' || data !== JSON.stringify(expected)) return false;
    const parsed = Reflect.apply(parseMessageMethod, messaging, [kind, data]);
    if (!parsed) return false;
    const expectedKeys = Object.keys(expected);
    for (let keyIndex = 0; keyIndex < expectedKeys.length; keyIndex += 1) {
      const key = expectedKeys[keyIndex];
      if (!key || parsed[key] !== expected[key]) return false;
    }
    return Object.keys(parsed).length === expectedKeys.length;
  };

  const bootstrapExpectation = () =>
    freeze({
      nonce: bootstrapNonce,
      attempt,
      generation: attemptGeneration,
      source: frameSource!,
      port: bootstrapListenerResource,
    });
  const rendererExpectation = (port: MessagingPort) =>
    freeze({
      nonce: rendererNonce,
      attempt,
      generation: attemptGeneration,
      source: frameSource!,
      port,
    });

  const receiveDocument = (event: unknown): void => {
    if (
      disposed ||
      !containerReady ||
      !envelopeSent ||
      !documentPort ||
      !frameSource ||
      !exactFrameBinding(true)
    ) {
      if (!disposed && containerReady && frameSource && !exactFrameBinding(true)) {
        fail(documentAccepted ? 'runner_failed' : 'renderer_document_no_load');
      }
      return;
    }
    const data = eventField(event, 'data');
    const accepted = Reflect.apply(parseMessageMethod, messaging, ['apsDocumentAccepted', data]);
    if (accepted?.['nonce'] === rendererNonce) {
      if (documentAccepted || attemptState() !== 'waiting_for_document') return;
      if (
        Reflect.apply(consumeRendererMethod, rendererNonces, [
          rendererExpectation(documentPort),
        ]) === true &&
        Reflect.apply(documentAcceptedMethod, attempt, []) === true
      ) {
        documentAccepted = true;
      } else {
        fail('renderer_document_no_load');
      }
      return;
    }
    const loaded = Reflect.apply(parseMessageMethod, messaging, ['apsRunnerLoaded', data]);
    if (loaded?.['nonce'] === rendererNonce) return;
    const completed = Reflect.apply(parseMessageMethod, messaging, ['apsRenderCompleted', data]);
    if (completed?.['nonce'] === rendererNonce) {
      if (!documentAccepted) return;
      if (mode === 'puc_overlay') {
        try {
          if (!exactFrameBinding(true) || !isBindingCurrent()) {
            fail('slot_unresolved');
            return;
          }
          iframe.style.setProperty('visibility', 'visible');
          if (iframe.style.getPropertyValue('visibility') !== 'visible') {
            fail('internal_error');
            return;
          }
        } catch {
          fail('internal_error');
          return;
        }
      }
      let artifactAssociationCommitted = false;
      if (mode === 'puc_overlay') {
        try {
          artifactAssociationCommitted = artifactBinding?.commit() === true;
        } catch {
          artifactAssociationCommitted = false;
        }
        if (!artifactAssociationCommitted) {
          fail('slot_unresolved');
          return;
        }
        if (!claimOverlayPositionLease()) {
          artifactBinding?.rollback();
          fail('slot_unresolved');
          return;
        }
      }
      const accepted = (() => {
        try {
          return Reflect.apply(acceptMethod, attempt, []) === true;
        } catch {
          return false;
        }
      })();
      if (!accepted) {
        if (artifactAssociationCommitted) artifactBinding?.rollback();
        if (!disposed) fail('internal_error');
        return;
      }
      if (artifactAssociationCommitted) artifactBinding?.finalize();
      if (!disposed && exactFrameBinding(true) && mode === 'direct') {
        commitContainer(insertionPredecessors);
      }
      return;
    }
    const failed = Reflect.apply(parseMessageMethod, messaging, ['apsRenderFailed', data]);
    if (failed?.['nonce'] !== rendererNonce) return;
    const reason = mapRunnerFailure(failed['reason']);
    if (reason) Reflect.apply(failMethod, attempt, [reason]);
  };

  const receiveDocumentError = (): void => {
    if (disposed || !containerReady) return;
    fail(documentAccepted ? 'runner_failed' : 'renderer_document_no_load');
  };

  const receiveGlobal = (event: MessageEvent): void => {
    if (
      disposed ||
      !frameSource ||
      eventField(event, 'source') !== frameSource ||
      eventField(event, 'origin') !== 'null'
    ) {
      return;
    }
    const data = eventField(event, 'data');

    if (!bootstrapBound) {
      const expectedReady = freeze({
        message: 'TS APS Bootstrap Ready',
        version: 1,
        bootstrapNonce,
      });
      if (!exactGlobalMessage('apsBootstrapReady', data, expectedReady)) return;
      const ports = Reflect.apply(extractPortsMethod, messaging, [event, 0]);
      if (!ports) {
        fail('renderer_document_no_load');
        return;
      }
      if (!exactFrameBinding(false) || attemptState() !== 'waiting_for_document') {
        fail('renderer_document_no_load');
        return;
      }
      if (Reflect.apply(bindBootstrapMethod, bootstrapNonces, [bootstrapExpectation()]) !== true) {
        fail('renderer_document_no_load');
        return;
      }
      bootstrapBound = true;
      try {
        Reflect.apply(elementSetAttributeIntrinsic, iframe, ['sandbox', APS_PERMANENT_SANDBOX]);
      } catch {
        fail('renderer_document_no_load');
        return;
      }
      if (!exactFrameBinding(true)) {
        fail('renderer_document_no_load');
        return;
      }
      const navigation = JSON.stringify({
        message: 'TS APS Bootstrap Configure',
        version: 2,
        bootstrapNonce,
        rendererNonce,
        creativeOrigin,
        tagType: renderer.tagType,
      });
      if (
        Reflect.apply(postWindowMethod, messaging, [frameSource, navigation, '*', []]) !== true ||
        !exactFrameBinding(true)
      ) {
        fail('renderer_document_no_load');
        return;
      }
      navigationPosted = true;
      return;
    }

    if (!navigationPosted || containerReady) return;
    const expectedContainerReady = freeze({
      message: 'TS APS Container Ready',
      version: 1,
      bootstrapNonce,
      rendererNonce,
    });
    if (!exactGlobalMessage('apsContainerReady', data, expectedContainerReady)) return;
    if (!exactFrameBinding(true) || attemptState() !== 'waiting_for_document') {
      fail('renderer_document_no_load');
      return;
    }
    const ports = Reflect.apply(extractPortsMethod, messaging, [event, 1]);
    const port = ports?.[0];
    if (!port) {
      fail('renderer_document_no_load');
      return;
    }
    documentPort = port;
    if (
      Reflect.apply(bindRendererMethod, rendererNonces, [rendererExpectation(port)]) !== true ||
      Reflect.apply(consumeBootstrapMethod, bootstrapNonces, [bootstrapExpectation()]) !== true
    ) {
      try {
        port.close();
      } catch {
        // A rejected transferred endpoint remains locally owned.
      }
      fail('renderer_document_no_load');
      return;
    }
    containerReady = true;
    bootstrapListenerResource.close();
    try {
      port.listen(receiveDocument, receiveDocumentError);
    } catch {
      fail('renderer_document_no_load');
      return;
    }
    const envelope = freeze({
      version: 1 as const,
      nonce: rendererNonce,
      publisherOrigin,
      renderer,
    });
    if (port.post(envelope, []) !== true || !exactFrameBinding(true)) {
      fail('renderer_document_no_load');
      return;
    }
    envelopeSent = true;
  };

  function onFrameError(): void {
    if (
      disposed ||
      (!insertionCommitted &&
        !(appendInProgress && Reflect.apply(nodeParentNodeGetter!, iframe, []) === container))
    ) {
      return;
    }
    frameErrorObserved = true;
    if (artifactOwnedByAttempt) {
      fail(documentAccepted ? 'runner_failed' : 'renderer_document_no_load');
    }
  }

  try {
    const initialState = mode === 'direct' ? 'rendering_direct' : 'waiting_for_insertion';
    releaseGlobalListener = Reflect.apply(installCaptureMethod, messaging, [receiveGlobal]);
    if (!releaseGlobalListener) return startupFailure('renderer_document_no_load');
    Reflect.apply(eventTargetAddListenerIntrinsic, iframe, ['error', onFrameError]);
    Reflect.apply(elementSetAttributeIntrinsic, iframe, ['src', expectedFrameSource]);
    if (
      Reflect.apply(elementGetAttributeIntrinsic, iframe, ['src']) !== expectedFrameSource ||
      Reflect.apply(iframeSourceGetter!, iframe, []) !== expectedFrameSource ||
      attemptState() !== initialState ||
      Reflect.apply(nodeIsConnectedGetter, container, []) !== true
    ) {
      return startupFailure('renderer_document_no_load');
    }
    insertionPredecessors = mode === 'direct' ? snapshotContainerPredecessors() : [];
    if (
      disposed ||
      attemptState() !== initialState ||
      Reflect.apply(nodeIsConnectedGetter, container, []) !== true ||
      !acquireOverlayPosition()
    ) {
      return startupFailure('renderer_document_no_load');
    }
    appendInProgress = true;
    try {
      Reflect.apply(nodeAppendChildIntrinsic, container, [iframe]);
    } finally {
      appendInProgress = false;
    }
    if (Reflect.apply(nodeParentNodeGetter, iframe, []) !== container) {
      return startupFailure('renderer_document_no_load');
    }
    insertionCommitted = true;
    const insertedSource = Reflect.apply(iframeContentWindowGetter, iframe, []) as Window | null;
    if (!insertedSource) {
      return startupFailure('renderer_document_no_load');
    }
    frameSource = insertedSource;
    if (!exactFrameBinding(false)) {
      return startupFailure('renderer_document_no_load');
    }
    if (Reflect.apply(beginDocumentMethod, attempt, [artifact]) !== true) {
      return startupFailure('internal_error');
    }
    artifactOwnedByAttempt = true;
    if (mode === 'puc_overlay') {
      try {
        onArtifactTransferred();
        baseArtifactTransferred = true;
      } catch {
        return fail('internal_error');
      }
    }
    if (
      disposed ||
      frameErrorObserved ||
      attemptState() !== 'waiting_for_document' ||
      !exactFrameBinding(false)
    ) {
      return fail('renderer_document_no_load');
    }
    return true;
  } catch {
    return startupFailure('renderer_document_no_load');
  }
}

/** Drive one direct APS attempt through the shared top-page mount service. */
export function renderDirectApsAttempt(options: DirectApsAttemptOptions): boolean {
  return mountApsTopPageAttempt({ ...options, mode: 'direct' });
}

/** Drive one PUC APS attempt through a hidden top-page overlay. */
export function renderPucApsAttempt(options: PucApsAttemptOptions): boolean {
  return mountApsTopPageAttempt({ ...options, mode: 'puc_overlay' });
}
