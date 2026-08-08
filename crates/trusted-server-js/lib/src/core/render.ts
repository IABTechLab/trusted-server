// Rendering utilities for injecting creatives into sandboxed iframes.
import NORMALIZE_CSS from './styles/normalize.css?inline';
import IFRAME_TEMPLATE from './templates/iframe.html?raw';

// Sandbox permissions granted to creative iframes.
//
// Ad creatives routinely contain scripts for impression reporting, click
// handling, and viewability measurement, so `allow-scripts` is required for
// them to render.
//
// `allow-same-origin` is deliberately excluded: combined with `allow-scripts` on
// srcdoc (or first-party src) content, that pair effectively removes the sandbox's
// origin isolation and would let SSP-provided markup run with the publisher
// origin's privileges — cookies, storage, and same-origin fetches. The origin
// boundary must not depend on server-side sanitization, which is optional
// (`auction.sanitize_creatives`) and cannot run at all for renderer-based bids.
// Matches APS_RENDERER_SANDBOX and ADM_IFRAME_SANDBOX, which already omit it.
const CREATIVE_SANDBOX_TOKENS = [
  'allow-forms',
  'allow-popups',
  'allow-popups-to-escape-sandbox',
  'allow-scripts',
  'allow-top-navigation-by-user-activation',
] as const;

/** Exact sandbox granted to TS-owned ADM documents. */
export const ADM_IFRAME_SANDBOX = CREATIVE_SANDBOX_TOKENS.join(' ');

const ADM_MAX_UTF8_BYTES = 512 * 1024;
const RENDER_DIMENSION_MIN = 1;
const RENDER_DIMENSION_MAX = 4096;
const nativeDocument = typeof document === 'undefined' ? undefined : document;
const nativeUrl = typeof URL === 'undefined' ? undefined : URL;
const nativeTextEncoder = typeof TextEncoder === 'undefined' ? undefined : TextEncoder;
const nativeTextEncoderEncode = nativeTextEncoder?.prototype.encode;
const nativePublisherOrigin =
  typeof location === 'undefined' ? undefined : exactHttpOrigin(location.origin);
const documentCreateElement =
  typeof Document === 'undefined' ? undefined : Document.prototype.createElement;
const nodeAppendChild = typeof Node === 'undefined' ? undefined : Node.prototype.appendChild;
const nodeRemoveChild = typeof Node === 'undefined' ? undefined : Node.prototype.removeChild;
const nodeParentGetter =
  typeof Node === 'undefined'
    ? undefined
    : Object.getOwnPropertyDescriptor(Node.prototype, 'parentNode')?.get;
const nodeOwnerDocumentGetter =
  typeof Node === 'undefined'
    ? undefined
    : Object.getOwnPropertyDescriptor(Node.prototype, 'ownerDocument')?.get;
const nodeConnectedGetter =
  typeof Node === 'undefined'
    ? undefined
    : Object.getOwnPropertyDescriptor(Node.prototype, 'isConnected')?.get;
const elementChildrenGetter =
  typeof Element === 'undefined'
    ? undefined
    : Object.getOwnPropertyDescriptor(Element.prototype, 'children')?.get;
const elementGetAttribute =
  typeof Element === 'undefined' ? undefined : Element.prototype.getAttribute;
const elementHasAttribute =
  typeof Element === 'undefined' ? undefined : Element.prototype.hasAttribute;
const elementSetAttribute =
  typeof Element === 'undefined' ? undefined : Element.prototype.setAttribute;
const eventTargetAddEventListener =
  typeof EventTarget === 'undefined' ? undefined : EventTarget.prototype.addEventListener;
const eventTargetRemoveEventListener =
  typeof EventTarget === 'undefined' ? undefined : EventTarget.prototype.removeEventListener;
const htmlCollectionLengthGetter =
  typeof HTMLCollection === 'undefined'
    ? undefined
    : Object.getOwnPropertyDescriptor(HTMLCollection.prototype, 'length')?.get;
const htmlCollectionItem =
  typeof HTMLCollection === 'undefined' ? undefined : HTMLCollection.prototype.item;
const iframeSrcdocDescriptor =
  typeof HTMLIFrameElement === 'undefined'
    ? undefined
    : Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'srcdoc');
const iframeReferrerPolicyDescriptor =
  typeof HTMLIFrameElement === 'undefined'
    ? undefined
    : Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'referrerPolicy');
const objectDefineProperty = Object.defineProperty;
const objectGetOwnPropertyDescriptor = Object.getOwnPropertyDescriptor;
const objectFreezeIntrinsic = Object.freeze;
const numberIsIntegerIntrinsic = Number.isInteger;
const reflectApplyIntrinsic = Reflect.apply;
const stringReplaceIntrinsic = String.prototype.replace;
const stringTrimIntrinsic = String.prototype.trim;
const stringIntrinsic = String;

export interface PrepareAdmIframeOptions {
  readonly adm: string;
  readonly container: HTMLElement;
  readonly height: number;
  readonly onError: () => void;
  readonly onLoad: () => void;
  readonly width: number;
}

export interface AdmIframeHandle {
  readonly frame: HTMLIFrameElement;
  append(): boolean;
  activate(): boolean;
  commit(): boolean;
  current(): boolean;
  dispose(): void;
}

function applyIntrinsic<Result>(
  method: (...arguments_: never[]) => unknown,
  receiver: unknown,
  arguments_: unknown[]
): Result {
  return reflectApplyIntrinsic(method, receiver, arguments_) as Result;
}

export type CreativeSanitizationRejectionReason = 'empty-after-sanitize' | 'invalid-creative-html';

export type AcceptedCreativeHtml = {
  kind: 'accepted';
  originalLength: number;
  sanitizedHtml: string;
  // Always equal to originalLength: the client validates type/emptiness only
  // and never removes content. Server-side sanitization is opt-in
  // (`auction.sanitize_creatives`); the origin boundary for this markup is the
  // iframe sandbox, not this function.
  // Retained so both union members of SanitizeCreativeHtmlResult have consistent fields.
  sanitizedLength: number;
  // Always 0 for the same reason — no content is removed client-side.
  removedCount: number;
};

export type RejectedCreativeHtml = {
  kind: 'rejected';
  originalLength: number;
  // Always equal to originalLength (or 0 for non-string input): no client-side
  // removal occurs. Retained so both union members of SanitizeCreativeHtmlResult have consistent fields.
  sanitizedLength: number;
  // Always 0 — no content is removed client-side.
  removedCount: number;
  rejectionReason: CreativeSanitizationRejectionReason;
};

export type SanitizeCreativeHtmlResult = AcceptedCreativeHtml | RejectedCreativeHtml;

// Validate the untrusted creative fragment before embedding it in the sandboxed iframe.
// This is validation-only, not sanitization: it guards against type errors and empty
// payloads and never removes content. Server-side stripping of executable markup is
// opt-in (`auction.sanitize_creatives`), so the adm arriving here may be raw bidder
// markup — the origin boundary is the iframe sandbox (no `allow-same-origin`), which
// does not depend on any sanitization having run. sanitizedLength always equals
// originalLength and removedCount is always 0 for accepted creatives — these fields
// exist for structural consistency with the shared result type but carry no signal here.
export function sanitizeCreativeHtml(creativeHtml: unknown): SanitizeCreativeHtmlResult {
  if (typeof creativeHtml !== 'string') {
    return {
      kind: 'rejected',
      originalLength: 0,
      sanitizedLength: 0,
      removedCount: 0,
      rejectionReason: 'invalid-creative-html',
    };
  }

  const originalLength = creativeHtml.length;

  if (creativeHtml.trim().length === 0) {
    return {
      kind: 'rejected',
      originalLength,
      sanitizedLength: originalLength,
      removedCount: 0,
      rejectionReason: 'empty-after-sanitize',
    };
  }

  return {
    kind: 'accepted',
    originalLength,
    sanitizedHtml: creativeHtml,
    sanitizedLength: originalLength,
    removedCount: 0,
  };
}

type IframeOptions = { name?: string; title?: string; width?: number; height?: number };

// Construct a sandboxed iframe for creative HTML. The markup may be raw bidder
// output (server-side sanitization is opt-in); the sandbox's origin isolation,
// not any sanitization, is the security boundary.
export function createAdIframe(
  container: HTMLElement,
  opts: IframeOptions = {}
): HTMLIFrameElement {
  const iframe = document.createElement('iframe');
  // Attributes
  iframe.scrolling = 'no';
  iframe.frameBorder = '0';
  iframe.setAttribute('marginwidth', '0');
  iframe.setAttribute('marginheight', '0');
  if (opts.name) iframe.name = String(opts.name);
  iframe.title = opts.title || 'Ad content';
  iframe.setAttribute('aria-label', 'Advertisement');
  // Sandbox permissions for creatives
  try {
    if (iframe.sandbox && typeof iframe.sandbox.add === 'function') {
      iframe.sandbox.add(...CREATIVE_SANDBOX_TOKENS);
    } else {
      iframe.setAttribute('sandbox', CREATIVE_SANDBOX_TOKENS.join(' '));
    }
  } catch {
    iframe.setAttribute('sandbox', CREATIVE_SANDBOX_TOKENS.join(' '));
  }
  // Sizing + style
  const w = Math.max(0, Number(opts.width ?? 0) | 0);
  const h = Math.max(0, Number(opts.height ?? 0) | 0);
  if (w > 0) iframe.width = String(w);
  if (h > 0) iframe.height = String(h);
  const s = iframe.style;
  s.setProperty('border', '0');
  s.setProperty('margin', '0');
  s.setProperty('overflow', 'hidden');
  s.setProperty('display', 'block');
  if (w > 0) s.setProperty('width', `${w}px`);
  if (h > 0) s.setProperty('height', `${h}px`);
  // Insert into container
  container.appendChild(iframe);
  return iframe;
}

// Origin the creative runtime resolves root-relative first-party URLs against.
//
// The srcdoc document has an opaque origin and an `about:srcdoc` location, so it
// has no usable origin of its own; `document.baseURI` would work but is
// inherited and honours a publisher `<base>`, i.e. it is not a trustworthy
// security boundary. This page — first-party, non-opaque — knows the real
// origin, so it stamps it into the document ahead of any creative markup.
//
// Only an exact `scheme://host[:port]` shape is emitted, so the value cannot
// break out of the quoted string it is written into.
function exactHttpOrigin(candidate: unknown): string | undefined {
  if (typeof candidate !== 'string' || !nativeUrl) return undefined;
  try {
    const parsed = new nativeUrl(candidate);
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return undefined;
    if (parsed.username !== '' || parsed.password !== '') return undefined;
    if (parsed.origin !== candidate) return undefined;
    return parsed.origin;
  } catch {
    return undefined;
  }
}

function trustedCreativeOrigin(): string {
  try {
    return exactHttpOrigin(location.origin) ?? '';
  } catch {
    // fall through to an empty stamp; the runtime degrades to document.baseURI
  }
  return '';
}

// Build a complete HTML document for a creative fragment, suitable for iframe.srcdoc.
export function buildCreativeDocument(
  creativeHtml: string,
  publisherOrigin: string = trustedCreativeOrigin()
): string {
  const normalized = applyIntrinsic<string>(stringReplaceIntrinsic, IFRAME_TEMPLATE, [
    '%NORMALIZE_CSS%',
    () => NORMALIZE_CSS,
  ]);
  const trusted = applyIntrinsic<string>(stringReplaceIntrinsic, normalized, [
    '%TRUSTED_ORIGIN%',
    () => exactHttpOrigin(publisherOrigin) ?? '',
  ]);
  return applyIntrinsic<string>(stringReplaceIntrinsic, trusted, [
    '%CREATIVE_HTML%',
    () => creativeHtml,
  ]);
}

function nativeParent(node: Node): Node | null | undefined {
  try {
    return nodeParentGetter ? applyIntrinsic<Node | null>(nodeParentGetter, node, []) : undefined;
  } catch {
    return undefined;
  }
}

function nativeOwnerDocument(node: Node): Document | null | undefined {
  try {
    return nodeOwnerDocumentGetter
      ? applyIntrinsic<Document | null>(nodeOwnerDocumentGetter, node, [])
      : undefined;
  } catch {
    return undefined;
  }
}

function nativeConnected(node: Node): boolean {
  try {
    return !!nodeConnectedGetter && applyIntrinsic(nodeConnectedGetter, node, []) === true;
  } catch {
    return false;
  }
}

function nativeAttribute(element: Element, name: string): string | null | undefined {
  try {
    return elementGetAttribute
      ? applyIntrinsic<string | null>(elementGetAttribute, element, [name])
      : undefined;
  } catch {
    return undefined;
  }
}

function hasNativeAttribute(element: Element, name: string): boolean {
  try {
    return (
      !!elementHasAttribute &&
      applyIntrinsic<boolean>(elementHasAttribute, element, [name]) === true
    );
  } catch {
    return true;
  }
}

function setNativeAttribute(element: Element, name: string, value: string): boolean {
  try {
    if (!elementSetAttribute) return false;
    applyIntrinsic(elementSetAttribute, element, [name, value]);
    return nativeAttribute(element, name) === value;
  } catch {
    return false;
  }
}

function nativeSrcdoc(frame: HTMLIFrameElement): string | undefined {
  try {
    return iframeSrcdocDescriptor?.get
      ? applyIntrinsic<string>(iframeSrcdocDescriptor.get, frame, [])
      : undefined;
  } catch {
    return undefined;
  }
}

function nativeReferrerPolicy(frame: HTMLIFrameElement): string | undefined {
  try {
    if (iframeReferrerPolicyDescriptor?.get) {
      return applyIntrinsic<string>(iframeReferrerPolicyDescriptor.get, frame, []);
    }
    const own = objectGetOwnPropertyDescriptor(frame, 'referrerPolicy');
    return own && 'value' in own && typeof own.value === 'string' ? own.value : undefined;
  } catch {
    return undefined;
  }
}

function removeNativeNode(node: Node): void {
  const parent = nativeParent(node);
  if (!parent || !nodeRemoveChild) return;
  try {
    applyIntrinsic(nodeRemoveChild, parent, [node]);
  } catch {
    // Best-effort disposal is intentionally exact to this owned node.
  }
}

function snapshotChildren(container: Element): Element[] | undefined {
  try {
    const children = elementChildrenGetter
      ? applyIntrinsic<HTMLCollection>(elementChildrenGetter, container, [])
      : undefined;
    if (!children || !htmlCollectionLengthGetter || !htmlCollectionItem) return undefined;
    const length = applyIntrinsic<number>(htmlCollectionLengthGetter, children, []);
    const snapshot: Element[] = [];
    for (let index = 0; index < length; index += 1) {
      const child = applyIntrinsic<Element | null>(htmlCollectionItem, children, [index]);
      if (!child) return undefined;
      snapshot[snapshot.length] = child;
    }
    return snapshot;
  } catch {
    return undefined;
  }
}

/**
 * Prepare one detached, fully configured ADM iframe.
 *
 * The returned handle owns insertion, event delivery, predecessor cleanup, and
 * disposal. No publisher-overridable instance methods are used for those actions.
 */
export function prepareAdmIframe(options: PrepareAdmIframeOptions): AdmIframeHandle | undefined {
  const { adm, container, height, onError, onLoad, width } = options;
  if (
    !nativeDocument ||
    !documentCreateElement ||
    !nodeAppendChild ||
    !nodeRemoveChild ||
    !eventTargetAddEventListener ||
    !eventTargetRemoveEventListener ||
    !iframeSrcdocDescriptor?.get ||
    !iframeSrcdocDescriptor.set ||
    nativeOwnerDocument(container) !== nativeDocument ||
    !nativeConnected(container) ||
    typeof adm !== 'string' ||
    applyIntrinsic<string>(stringTrimIntrinsic, adm, []).length === 0 ||
    !nativeTextEncoder ||
    !nativeTextEncoderEncode ||
    !applyIntrinsic<boolean>(numberIsIntegerIntrinsic, Number, [width]) ||
    width < RENDER_DIMENSION_MIN ||
    width > RENDER_DIMENSION_MAX ||
    !applyIntrinsic<boolean>(numberIsIntegerIntrinsic, Number, [height]) ||
    height < RENDER_DIMENSION_MIN ||
    height > RENDER_DIMENSION_MAX ||
    typeof onLoad !== 'function' ||
    typeof onError !== 'function'
  ) {
    return undefined;
  }

  try {
    const encoder = new nativeTextEncoder();
    const bytes = applyIntrinsic<Uint8Array>(nativeTextEncoderEncode, encoder, [adm]);
    if (bytes.byteLength > ADM_MAX_UTF8_BYTES) return undefined;
  } catch {
    return undefined;
  }

  let frame: HTMLIFrameElement;
  try {
    frame = applyIntrinsic<HTMLIFrameElement>(documentCreateElement, nativeDocument, ['iframe']);
  } catch {
    return undefined;
  }
  if (nativeOwnerDocument(frame) !== nativeDocument || nativeParent(frame) !== null)
    return undefined;

  const intendedSrcdoc = buildCreativeDocument(adm, nativePublisherOrigin ?? '');
  const attributes = [
    ['sandbox', ADM_IFRAME_SANDBOX],
    ['referrerpolicy', 'no-referrer'],
    ['width', applyIntrinsic<string>(stringIntrinsic, undefined, [width])],
    ['height', applyIntrinsic<string>(stringIntrinsic, undefined, [height])],
    ['scrolling', 'no'],
    ['frameborder', '0'],
    ['marginwidth', '0'],
    ['marginheight', '0'],
    ['title', 'Ad content'],
    ['aria-label', 'Advertisement'],
    [
      'style',
      `border: 0; margin: 0; overflow: hidden; display: block; width: ${width}px; height: ${height}px;`,
    ],
  ] as const;
  for (let index = 0; index < attributes.length; index += 1) {
    const attribute = attributes[index];
    if (!attribute) return undefined;
    const name = attribute[0];
    const value = attribute[1];
    if (!setNativeAttribute(frame, name, value)) return undefined;
  }
  try {
    if (iframeReferrerPolicyDescriptor?.set) {
      applyIntrinsic(iframeReferrerPolicyDescriptor.set, frame, ['no-referrer']);
    } else {
      objectDefineProperty(frame, 'referrerPolicy', {
        configurable: false,
        enumerable: true,
        value: 'no-referrer',
        writable: false,
      });
    }
  } catch {
    return undefined;
  }

  let active = false;
  let appended = false;
  let committed = false;
  let disposed = false;
  let terminal = false;
  let pending: 'error' | 'load' | undefined;
  let predecessors: Element[] = [];

  const exactAttributes = (): boolean => {
    for (let index = 0; index < attributes.length; index += 1) {
      const attribute = attributes[index];
      if (!attribute) return false;
      const name = attribute[0];
      const value = attribute[1];
      if (nativeAttribute(frame, name) !== value) return false;
    }
    return nativeReferrerPolicy(frame) === 'no-referrer';
  };

  const current = (): boolean => {
    if (
      disposed ||
      !appended ||
      nativeParent(frame) !== container ||
      nativeOwnerDocument(frame) !== nativeDocument ||
      !nativeConnected(frame) ||
      nativeSrcdoc(frame) !== intendedSrcdoc ||
      hasNativeAttribute(frame, 'src')
    ) {
      return false;
    }
    return exactAttributes();
  };

  const removeListeners = (): void => {
    try {
      applyIntrinsic(eventTargetRemoveEventListener, frame, ['load', onFrameLoad]);
      applyIntrinsic(eventTargetRemoveEventListener, frame, ['error', onFrameError]);
    } catch {
      // Listener disposal remains best-effort after a hostile realm mutation.
    }
  };

  const settle = (outcome: 'error' | 'load'): void => {
    if (disposed || terminal) return;
    terminal = true;
    pending = undefined;
    removeListeners();
    if (outcome === 'load' && current()) onLoad();
    else onError();
  };

  function onFrameLoad(): void {
    if (disposed || terminal || !appended) return;
    if (!current()) {
      if (active) settle('error');
      else pending = 'error';
      return;
    }
    if (active) settle('load');
    else pending = 'load';
  }

  function onFrameError(): void {
    if (disposed || terminal || !appended) return;
    if (active) settle('error');
    else pending = 'error';
  }

  try {
    applyIntrinsic(eventTargetAddEventListener, frame, ['load', onFrameLoad]);
    applyIntrinsic(eventTargetAddEventListener, frame, ['error', onFrameError]);
    applyIntrinsic(iframeSrcdocDescriptor.set, frame, [intendedSrcdoc]);
  } catch {
    removeListeners();
    return undefined;
  }
  if (nativeSrcdoc(frame) !== intendedSrcdoc || hasNativeAttribute(frame, 'src')) {
    removeListeners();
    return undefined;
  }

  const dispose = (): void => {
    if (disposed) return;
    disposed = true;
    pending = undefined;
    removeListeners();
    removeNativeNode(frame);
  };

  return applyIntrinsic<Readonly<AdmIframeHandle>>(objectFreezeIntrinsic, Object, [
    {
      frame,
      append: (): boolean => {
        if (
          disposed ||
          committed ||
          appended ||
          nativeParent(frame) !== null ||
          nativeOwnerDocument(container) !== nativeDocument ||
          !nativeConnected(container) ||
          nativeSrcdoc(frame) !== intendedSrcdoc ||
          hasNativeAttribute(frame, 'src')
        ) {
          return false;
        }
        const before = snapshotChildren(container);
        if (!before) return false;
        predecessors = before;
        appended = true;
        try {
          applyIntrinsic(nodeAppendChild, container, [frame]);
        } catch {
          dispose();
          return false;
        }
        if (!current()) {
          dispose();
          return false;
        }
        return true;
      },
      activate: (): boolean => {
        if (disposed || committed || terminal || active || !appended) return false;
        active = true;
        if (!current()) settle('error');
        else if (pending) settle(pending);
        return true;
      },
      commit: (): boolean => {
        if (disposed || committed || !terminal || !current()) return false;
        removeListeners();
        for (let index = 0; index < predecessors.length; index += 1) {
          const predecessor = predecessors[index];
          if (!predecessor || !current()) return false;
          if (predecessor !== frame && nativeParent(predecessor) === container) {
            removeNativeNode(predecessor);
            if (nativeParent(predecessor) === container) return false;
          }
        }
        if (!current()) return false;
        predecessors = [];
        committed = true;
        return true;
      },
      current,
      dispose,
    },
  ]);
}
