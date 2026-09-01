type BrowserRouteKind = 'script' | 'preload' | 'prefetch' | 'beacon' | 'fetch';

export interface FirstDisplayBrowserRouteRuleV1 {
  readonly matches: (kind: BrowserRouteKind, url: string) => boolean;
  readonly rewrite: (url: string) => string;
}

export interface FirstDisplayBrowserRouteOwnerV1 {
  readonly register: (rule: FirstDisplayBrowserRouteRuleV1, network?: boolean) => () => void;
  readonly dispose: () => void;
}

interface RouteRegistration {
  readonly rule: FirstDisplayBrowserRouteRuleV1;
  readonly network: boolean;
}

function restoreOwnedProperty(
  owner: object,
  key: PropertyKey,
  installed: unknown,
  original: unknown
): void {
  try {
    if (Reflect.get(owner, key) === installed) Reflect.set(owner, key, original);
  } catch {
    // Browser-owned or publisher-hardened surfaces may reject restoration.
    // Every other surface must still receive its independent best-effort restore.
  }
}

function route(
  registrations: readonly RouteRegistration[],
  kind: BrowserRouteKind,
  url: string
): string {
  let current = url;
  for (const registration of registrations) {
    if ((kind === 'beacon' || kind === 'fetch') && !registration.network) continue;
    try {
      if (registration.rule.matches(kind, current)) current = registration.rule.rewrite(current);
    } catch {
      // A malformed integration rule cannot suppress the remaining route owners.
    }
  }
  return current;
}

/** Create one document-scoped parser-time route owner shared by every selected slice. */
export function createFirstDisplayBrowserRouteOwner(
  routeDocument: Document,
  browser: Window
): FirstDisplayBrowserRouteOwnerV1 {
  const registrations: RouteRegistration[] = [];
  const ElementConstructor = routeDocument.defaultView?.Element ?? Element;
  const prototype = ElementConstructor.prototype;
  const appendChild = prototype.appendChild;
  const insertBefore = prototype.insertBefore;
  const append = prototype.append;
  const prepend = prototype.prepend;
  const replaceChildren = prototype.replaceChildren;
  const navigator = browser.navigator;
  const sendBeacon = navigator.sendBeacon;
  const fetch = browser.fetch;
  let disposed = false;
  let networkInstalled = false;

  const rewriteNode = (node: Node): void => {
    if (node.nodeType !== Node.ELEMENT_NODE) return;
    const element = node as Element;
    if (element.tagName === 'SCRIPT') {
      const script = element as HTMLScriptElement;
      const source = script.getAttribute('src') ?? script.src;
      const rewritten = source && route(registrations, 'script', source);
      if (rewritten && rewritten !== source) script.src = rewritten;
      return;
    }
    if (element.tagName !== 'LINK') return;
    const link = element as HTMLLinkElement;
    const rel = link.getAttribute('rel');
    if ((rel !== 'preload' && rel !== 'prefetch') || link.getAttribute('as') !== 'script') return;
    const source = link.getAttribute('href') ?? link.href;
    const rewritten = source && route(registrations, rel, source);
    if (rewritten && rewritten !== source) link.href = rewritten;
  };
  const rewriteTree = (node: Node): void => {
    rewriteNode(node);
    if (node.nodeType !== Node.ELEMENT_NODE && node.nodeType !== Node.DOCUMENT_FRAGMENT_NODE)
      return;
    for (const nested of (node as Element | DocumentFragment).querySelectorAll(
      'script[src],link[href]'
    )) {
      rewriteNode(nested);
    }
  };
  const rewriteVariadic = (nodes: readonly (Node | string)[]): void => {
    for (const node of nodes) {
      if (typeof node !== 'string') rewriteTree(node);
    }
  };
  const appendWrapper: typeof prototype.appendChild = function <T extends Node>(
    this: Element,
    node: T
  ): T {
    rewriteTree(node);
    return Reflect.apply(appendChild, this, [node]) as T;
  };
  const insertWrapper: typeof prototype.insertBefore = function <T extends Node>(
    this: Element,
    node: T,
    child: Node | null
  ): T {
    rewriteTree(node);
    return Reflect.apply(insertBefore, this, [node, child]) as T;
  };
  const appendVariadicWrapper: typeof prototype.append = function (
    this: Element,
    ...nodes: (Node | string)[]
  ): void {
    rewriteVariadic(nodes);
    Reflect.apply(append, this, nodes);
  };
  const prependWrapper: typeof prototype.prepend = function (
    this: Element,
    ...nodes: (Node | string)[]
  ): void {
    rewriteVariadic(nodes);
    Reflect.apply(prepend, this, nodes);
  };
  const replaceChildrenWrapper: typeof prototype.replaceChildren = function (
    this: Element,
    ...nodes: (Node | string)[]
  ): void {
    rewriteVariadic(nodes);
    Reflect.apply(replaceChildren, this, nodes);
  };
  const restoreDom = (): void => {
    restoreOwnedProperty(prototype, 'appendChild', appendWrapper, appendChild);
    restoreOwnedProperty(prototype, 'insertBefore', insertWrapper, insertBefore);
    restoreOwnedProperty(prototype, 'append', appendVariadicWrapper, append);
    restoreOwnedProperty(prototype, 'prepend', prependWrapper, prepend);
    restoreOwnedProperty(prototype, 'replaceChildren', replaceChildrenWrapper, replaceChildren);
  };

  const Observer = routeDocument.defaultView?.MutationObserver;
  let observer: MutationObserver | undefined;
  try {
    prototype.appendChild = appendWrapper;
    prototype.insertBefore = insertWrapper;
    prototype.append = appendVariadicWrapper;
    prototype.prepend = prependWrapper;
    prototype.replaceChildren = replaceChildrenWrapper;
    observer = Observer
      ? new Observer((records) => {
          for (const record of records) {
            for (const node of record.addedNodes) rewriteTree(node);
          }
        })
      : undefined;
    observer?.observe(routeDocument.documentElement, { childList: true, subtree: true });
  } catch (error) {
    try {
      observer?.disconnect();
    } catch {
      // Continue restoring every installed insertion surface.
    }
    restoreDom();
    throw error;
  }

  const beaconWrapper = function (url: string | URL, data?: BodyInit | null): boolean {
    return Reflect.apply(sendBeacon, navigator, [
      route(registrations, 'beacon', String(url)),
      data,
    ]);
  };
  const fetchWrapper = function (input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
    let source: string | undefined;
    if (typeof input === 'string' || input instanceof URL) source = String(input);
    else if (typeof Request !== 'undefined' && input instanceof Request) source = input.url;
    const rewritten = source && route(registrations, 'fetch', source);
    let request = input;
    if (rewritten && rewritten !== source) {
      if (typeof Request !== 'undefined' && input instanceof Request) {
        const requestInit: RequestInit & { duplex?: 'half' } = {
          cache: input.cache,
          credentials: input.credentials,
          headers: input.headers,
          integrity: input.integrity,
          keepalive: input.keepalive,
          method: input.method,
          mode: input.mode,
          redirect: input.redirect,
          referrer: input.referrer,
          referrerPolicy: input.referrerPolicy,
          signal: input.signal,
        };
        if (input.body !== null && input.method !== 'GET' && input.method !== 'HEAD') {
          requestInit.body = input.body as BodyInit;
          requestInit.duplex = 'half';
        }
        request = new Request(rewritten, requestInit);
      } else {
        request = rewritten;
      }
    }
    return Reflect.apply(fetch, browser, [request, init]);
  };

  const restoreNetwork = (): void => {
    if (!networkInstalled) return;
    networkInstalled = false;
    restoreOwnedProperty(navigator, 'sendBeacon', beaconWrapper, sendBeacon);
    restoreOwnedProperty(browser, 'fetch', fetchWrapper, fetch);
  };
  const installNetwork = (): void => {
    if (networkInstalled) return;
    try {
      if (typeof sendBeacon === 'function') navigator.sendBeacon = beaconWrapper;
      if (typeof fetch === 'function') browser.fetch = fetchWrapper;
      networkInstalled = true;
    } catch (error) {
      restoreOwnedProperty(navigator, 'sendBeacon', beaconWrapper, sendBeacon);
      restoreOwnedProperty(browser, 'fetch', fetchWrapper, fetch);
      throw error;
    }
  };
  const dispose = (): void => {
    if (disposed) return;
    disposed = true;
    registrations.length = 0;
    try {
      observer?.disconnect();
    } catch {
      // Generation latching makes an unremovable observer inert.
    }
    restoreDom();
    restoreNetwork();
  };

  return Object.freeze({
    register: (rule: FirstDisplayBrowserRouteRuleV1, network = false): (() => void) => {
      if (
        disposed ||
        !rule ||
        typeof rule.matches !== 'function' ||
        typeof rule.rewrite !== 'function'
      ) {
        throw new TypeError('tsjs');
      }
      const registration = Object.freeze({ rule, network });
      registrations.push(registration);
      try {
        if (network) installNetwork();
      } catch (error) {
        registrations.pop();
        if (registrations.length === 0) dispose();
        throw error;
      }
      let active = true;
      return (): void => {
        if (!active) return;
        active = false;
        const index = registrations.indexOf(registration);
        if (index >= 0) registrations.splice(index, 1);
        if (!registrations.some((entry) => entry.network)) restoreNetwork();
        if (registrations.length === 0) dispose();
      };
    },
    dispose,
  });
}

let defaultOwner: FirstDisplayBrowserRouteOwnerV1 | undefined;
let defaultRegistrations = 0;

/** Register through the document singleton used by direct unit consumers. */
export function registerFirstDisplayBrowserRoute(
  rule: FirstDisplayBrowserRouteRuleV1,
  network = false
): () => void {
  if (!defaultOwner) defaultOwner = createFirstDisplayBrowserRouteOwner(document, window);
  const owner = defaultOwner;
  const release = owner.register(rule, network);
  defaultRegistrations += 1;
  let active = true;
  return (): void => {
    if (!active) return;
    active = false;
    release();
    defaultRegistrations -= 1;
    if (defaultRegistrations === 0 && defaultOwner === owner) defaultOwner = undefined;
  };
}
