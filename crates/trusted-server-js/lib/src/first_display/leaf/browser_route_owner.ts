type BrowserRouteKind = 'script' | 'preload' | 'prefetch' | 'beacon' | 'fetch';

export interface FirstDisplayBrowserRouteRuleV1 {
  readonly matches: (kind: BrowserRouteKind, url: string) => boolean;
  readonly rewrite: (url: string) => string;
}

function route(rule: FirstDisplayBrowserRouteRuleV1, kind: BrowserRouteKind, url: string): string {
  try {
    return rule.matches(kind, url) ? rule.rewrite(url) : url;
  } catch {
    return url;
  }
}

/** Own one compact parser-time route interceptor until takeover or rollback. */
export function registerFirstDisplayBrowserRoute(
  rule: FirstDisplayBrowserRouteRuleV1,
  network = false
): () => void {
  const prototype = Element.prototype;
  const appendChild = prototype.appendChild;
  const insertBefore = prototype.insertBefore;
  const rewriteNode = (node: Node): void => {
    if (node.nodeType !== Node.ELEMENT_NODE) return;
    const element = node as Element;
    if (element.tagName === 'SCRIPT') {
      const script = element as HTMLScriptElement;
      const source = script.getAttribute('src') ?? script.src;
      const rewritten = source && route(rule, 'script', source);
      if (rewritten && rewritten !== source) script.src = rewritten;
      return;
    }
    if (element.tagName !== 'LINK') return;
    const link = element as HTMLLinkElement;
    const rel = link.getAttribute('rel');
    if ((rel !== 'preload' && rel !== 'prefetch') || link.getAttribute('as') !== 'script') return;
    const source = link.getAttribute('href') ?? link.href;
    const rewritten = source && route(rule, rel, source);
    if (rewritten && rewritten !== source) link.href = rewritten;
  };
  const appendWrapper: typeof prototype.appendChild = function <T extends Node>(
    this: Element,
    node: T
  ): T {
    rewriteNode(node);
    return Reflect.apply(appendChild, this, [node]) as T;
  };
  const insertWrapper: typeof prototype.insertBefore = function <T extends Node>(
    this: Element,
    node: T,
    child: Node | null
  ): T {
    rewriteNode(node);
    return Reflect.apply(insertBefore, this, [node, child]) as T;
  };
  prototype.appendChild = appendWrapper;
  prototype.insertBefore = insertWrapper;

  const Observer = document.defaultView?.MutationObserver;
  const observer = Observer
    ? new Observer((records) => {
        for (const record of records) {
          for (const node of record.addedNodes) {
            rewriteNode(node);
            if (node.nodeType === Node.ELEMENT_NODE) {
              for (const nested of (node as Element).querySelectorAll('script[src],link[href]')) {
                rewriteNode(nested);
              }
            }
          }
        }
      })
    : undefined;
  observer?.observe(document.documentElement, { childList: true, subtree: true });

  const navigator = window.navigator;
  const sendBeacon = navigator.sendBeacon;
  const fetch = window.fetch;
  const beaconWrapper = function (url: string | URL, data?: BodyInit | null): boolean {
    return Reflect.apply(sendBeacon, navigator, [route(rule, 'beacon', String(url)), data]);
  };
  const fetchWrapper = function (input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
    let source: string | undefined;
    if (typeof input === 'string' || input instanceof URL) source = String(input);
    else if (typeof Request !== 'undefined' && input instanceof Request) source = input.url;
    const rewritten = source && route(rule, 'fetch', source);
    const request =
      rewritten &&
      rewritten !== source &&
      typeof Request !== 'undefined' &&
      input instanceof Request
        ? new Request(rewritten, input)
        : (rewritten ?? input);
    return Reflect.apply(fetch, window, [request, init]);
  };
  if (network) {
    if (typeof sendBeacon === 'function') navigator.sendBeacon = beaconWrapper;
    if (typeof fetch === 'function') window.fetch = fetchWrapper;
  }

  let active = true;
  return (): void => {
    if (!active) return;
    active = false;
    observer?.disconnect();
    if (prototype.appendChild === appendWrapper) prototype.appendChild = appendChild;
    if (prototype.insertBefore === insertWrapper) prototype.insertBefore = insertBefore;
    if (network && navigator.sendBeacon === beaconWrapper) navigator.sendBeacon = sendBeacon;
    if (network && window.fetch === fetchWrapper) window.fetch = fetch;
  };
}
