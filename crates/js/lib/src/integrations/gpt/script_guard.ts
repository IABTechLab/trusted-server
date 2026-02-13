import { log } from '../../core/log';

/**
 * GPT Script Interception Guard
 *
 * Intercepts script elements whose URLs point at Google's ad-serving domains
 * and synchronously rewrites them to the first-party proxy, preserving the
 * original path. This guard performs a *host swap*:
 *
 *   securepubads.g.doubleclick.net/pagead/managed/js/gpt/…/pubads_impl.js
 *   → publisher.com/integrations/gpt/pagead/managed/js/gpt/…/pubads_impl.js
 *
 * The server-side proxy serves script bodies verbatim, so this guard is
 * the sole mechanism that routes GPT's cascaded script loads (pubads_impl,
 * sub-modules, viewability, etc.) back through the first-party proxy.
 *
 * ## Interception layers
 *
 * 1. **`document.write` / `document.writeln`** — GPT's primary loading
 *    mechanism. When gpt.js loads synchronously it uses `document.write`
 *    to inject `<script src="...pubads_impl.js">` directly into the
 *    HTML parser stream. We intercept these calls and rewrite GPT domain
 *    URLs inside the HTML string before passing it to the native method.
 * 2. **Property descriptor** on `HTMLScriptElement.prototype.src` — catches
 *    `script.src = url` (the async fallback path GPT uses when
 *    `document.write` is unavailable).
 * 3. **`setAttribute` patch** on `HTMLScriptElement.prototype` — catches
 *    `script.setAttribute('src', url)`.
 * 4. **`document.createElement` patch** — tags every newly created
 *    `<script>` element with a per-instance `src` descriptor as a
 *    fallback when the prototype descriptor cannot be installed.
 * 5. **DOM insertion patches** on `appendChild` / `insertBefore` — catches
 *    scripts and `<link rel="preload">` elements at insertion time.
 * 6. **`MutationObserver`** — catches elements added to the DOM via
 *    `innerHTML`, `.append()`, etc., or attribute mutations on existing
 *    elements.
 */

const LOG_PREFIX = 'GPT guard';

/** The Google ad-serving domain whose scripts should be proxied. */
const GPT_DOMAIN = 'securepubads.g.doubleclick.net';

/** Integration route prefix on the first-party domain. */
const PROXY_PREFIX = '/integrations/gpt';

// ---------------------------------------------------------------------------
// URL helpers
// ---------------------------------------------------------------------------

/**
 * Parse a URL string into a URL object, supporting protocol-relative URLs.
 */
function parseUrl(url: string): URL | undefined {
  if (!url) return undefined;

  try {
    if (typeof window !== 'undefined') {
      return new URL(url, window.location.href);
    }
    return new URL(url);
  } catch {
    if (!url.startsWith('//')) {
      return undefined;
    }

    try {
      return new URL(`https:${url}`);
    } catch {
      return undefined;
    }
  }
}

/**
 * Check if a URL belongs to one of Google's GPT / ad-serving domains.
 */
function isGptDomainUrl(url: string): boolean {
  const parsed = parseUrl(url);
  return !!parsed && parsed.hostname.toLowerCase() === GPT_DOMAIN;
}

/**
 * Rewrite a GPT URL to the first-party proxy, preserving the path.
 *
 * ```
 * https://securepubads.g.doubleclick.net/pagead/managed/…
 * → https://publisher.com/integrations/gpt/pagead/managed/…
 * ```
 */
function rewriteUrl(originalUrl: string): string {
  const parsed = parseUrl(originalUrl);
  if (!parsed || typeof window === 'undefined') {
    return originalUrl;
  }

  const protocol = window.location.protocol === 'https:' ? 'https' : 'http';
  const host = window.location.host;
  return `${protocol}://${host}${PROXY_PREFIX}${parsed.pathname}${parsed.search}${parsed.hash}`;
}

// ---------------------------------------------------------------------------
// Native references (captured once at install time)
// ---------------------------------------------------------------------------

let nativeDocWrite: ((this: Document, ...args: string[]) => void) | undefined;
let nativeDocWriteln: ((this: Document, ...args: string[]) => void) | undefined;
let nativeSrcSet: ((this: HTMLScriptElement, value: string) => void) | undefined;
let nativeSrcGet: ((this: HTMLScriptElement) => string) | undefined;
let nativeSrcDescriptor: PropertyDescriptor | undefined;
let nativeSetAttribute: typeof HTMLScriptElement.prototype.setAttribute | undefined;
let nativeCreateElement: typeof document.createElement | undefined;
let nativeAppendChild: typeof Element.prototype.appendChild | undefined;
let nativeInsertBefore: typeof Element.prototype.insertBefore | undefined;
let mutationObserver: MutationObserver | undefined;

// ---------------------------------------------------------------------------
// Tracking — prevent double-rewriting
// ---------------------------------------------------------------------------

/** Elements whose src we've already rewritten. Value is the rewritten URL. */
let rewritten = new WeakMap<HTMLScriptElement | HTMLLinkElement, string>();

function alreadyRewritten(element: HTMLScriptElement | HTMLLinkElement, url: string): boolean {
  return rewritten.get(element) === url;
}

// ---------------------------------------------------------------------------
// Core rewrite logic
// ---------------------------------------------------------------------------

/**
 * Apply a rewritten URL to a script element using the native setter,
 * bypassing our property descriptor.
 */
function applySrc(element: HTMLScriptElement, url: string): void {
  if (nativeSrcSet) {
    nativeSrcSet.call(element, url);
  } else if (nativeSetAttribute) {
    nativeSetAttribute.call(element, 'src', url);
  } else {
    element.setAttribute('src', url);
  }
}

/**
 * If the URL is a GPT domain URL, rewrite it. Returns the (possibly
 * rewritten) URL and whether a rewrite was performed.
 */
function maybeRewrite(url: string): { url: string; didRewrite: boolean } {
  if (!isGptDomainUrl(url)) {
    return { url, didRewrite: false };
  }
  return { url: rewriteUrl(url), didRewrite: true };
}

/**
 * Attempt to rewrite a script element's src if it points at a GPT domain.
 */
function rewriteScriptSrc(element: HTMLScriptElement, rawUrl: string): void {
  const { url: finalUrl, didRewrite } = maybeRewrite(rawUrl);
  if (!didRewrite) return;
  if (alreadyRewritten(element, finalUrl)) return;

  log.info(`${LOG_PREFIX}: rewriting script src`, { original: rawUrl, rewritten: finalUrl });
  rewritten.set(element, finalUrl);
  applySrc(element, finalUrl);
}

/**
 * Attempt to rewrite a link element's href if it's a preload/prefetch for
 * a GPT-domain script.
 */
function rewriteLinkHref(element: HTMLLinkElement): void {
  const rel = element.getAttribute('rel');
  if (rel !== 'preload' && rel !== 'prefetch') return;
  if (element.getAttribute('as') !== 'script') return;

  const href = element.href || element.getAttribute('href') || '';
  const { url: finalUrl, didRewrite } = maybeRewrite(href);
  if (!didRewrite) return;
  if (alreadyRewritten(element, finalUrl)) return;

  log.info(`${LOG_PREFIX}: rewriting ${rel} link`, { original: href, rewritten: finalUrl });
  rewritten.set(element, finalUrl);
  element.href = finalUrl;
}

// ---------------------------------------------------------------------------
// Layer 1: document.write / document.writeln interception
// ---------------------------------------------------------------------------

/**
 * Regex that matches `src="..."` or `src='...'` attributes inside a
 * `<script>` tag where the URL contains a GPT domain. We capture:
 *   1. Everything before the URL (the `src=` prefix with quote)
 *   2. The URL itself
 *   3. Everything after the URL (the closing quote)
 *
 * This handles the HTML that GPT's `Xd` function produces, e.g.:
 *   `<script src="https://securepubads.g.doubleclick.net/pagead/…/pubads_impl.js" …></script>`
 */
const SCRIPT_SRC_RE =
  /(<script\b[^>]*?\bsrc\s*=\s*["'])([^"']*securepubads\.g\.doubleclick\.net[^"']*)(["'])/gi;

/**
 * Rewrite GPT domain URLs inside raw HTML strings passed to
 * `document.write` / `document.writeln`.
 */
function rewriteHtmlString(html: string): string {
  SCRIPT_SRC_RE.lastIndex = 0;
  if (!SCRIPT_SRC_RE.test(html)) return html;
  SCRIPT_SRC_RE.lastIndex = 0;

  return html.replace(SCRIPT_SRC_RE, (_match, prefix: string, url: string, suffix: string) => {
    const rewrittenUrl = rewriteUrl(url);
    log.info(`${LOG_PREFIX}: rewriting document.write script src`, {
      original: url,
      rewritten: rewrittenUrl,
    });
    return `${prefix}${rewrittenUrl}${suffix}`;
  });
}

function installDocumentWritePatch(): void {
  if (typeof document === 'undefined') return;

  nativeDocWrite = document.write;
  nativeDocWriteln = document.writeln;

  document.write = function patchedWrite(this: Document, ...args: string[]): void {
    const rewrittenArgs = args.map((arg) =>
      typeof arg === 'string' ? rewriteHtmlString(arg) : arg
    );
    nativeDocWrite!.apply(this, rewrittenArgs);
  };

  document.writeln = function patchedWriteln(this: Document, ...args: string[]): void {
    const rewrittenArgs = args.map((arg) =>
      typeof arg === 'string' ? rewriteHtmlString(arg) : arg
    );
    nativeDocWriteln!.apply(this, rewrittenArgs);
  };

  log.info(`${LOG_PREFIX}: document.write/writeln patch installed`);
}

// ---------------------------------------------------------------------------
// Layer 2: Property descriptor on HTMLScriptElement.prototype.src
// ---------------------------------------------------------------------------

function installSrcDescriptor(): boolean {
  if (typeof HTMLScriptElement === 'undefined') return false;

  const descriptor = Object.getOwnPropertyDescriptor(HTMLScriptElement.prototype, 'src');
  if (!descriptor || typeof descriptor.set !== 'function') {
    log.debug(`${LOG_PREFIX}: HTMLScriptElement.prototype.src has no setter, skipping descriptor`);
    return false;
  }
  if (descriptor.configurable === false) {
    log.debug(`${LOG_PREFIX}: HTMLScriptElement.prototype.src is not configurable`);
    return false;
  }

  nativeSrcDescriptor = descriptor;
  nativeSrcSet = descriptor.set;
  nativeSrcGet = typeof descriptor.get === 'function' ? descriptor.get : undefined;

  try {
    Object.defineProperty(HTMLScriptElement.prototype, 'src', {
      configurable: true,
      enumerable: descriptor.enumerable ?? true,
      get(this: HTMLScriptElement): string {
        if (nativeSrcGet) {
          return nativeSrcGet.call(this);
        }
        return this.getAttribute('src') ?? '';
      },
      set(this: HTMLScriptElement, value: string) {
        const raw = String(value ?? '');
        const { url: finalUrl, didRewrite } = maybeRewrite(raw);
        if (didRewrite && !alreadyRewritten(this, finalUrl)) {
          log.info(`${LOG_PREFIX}: intercepted src setter`, { original: raw, rewritten: finalUrl });
          rewritten.set(this, finalUrl);
          applySrc(this, finalUrl);
        } else {
          applySrc(this, raw);
        }
      },
    });
    log.info(`${LOG_PREFIX}: src property descriptor installed`);
    return true;
  } catch (err) {
    log.debug(`${LOG_PREFIX}: failed to install src descriptor`, err);
    return false;
  }
}

// ---------------------------------------------------------------------------
// Layer 3: setAttribute patch on HTMLScriptElement.prototype
// ---------------------------------------------------------------------------

// Track instance-level src patching to avoid redundant work.
let instancePatched = new WeakSet<HTMLScriptElement>();

/**
 * Install a per-instance `src` property descriptor on a script element.
 * Used as a fallback when the prototype-level descriptor cannot be
 * installed, or as belt-and-suspenders from `document.createElement`.
 */
function ensureInstancePatched(element: HTMLScriptElement): void {
  if (instancePatched.has(element)) return;
  instancePatched.add(element);

  try {
    Object.defineProperty(element, 'src', {
      configurable: true,
      enumerable: true,
      get(this: HTMLScriptElement): string {
        if (nativeSrcGet) {
          return nativeSrcGet.call(this);
        }
        return this.getAttribute('src') ?? '';
      },
      set(this: HTMLScriptElement, value: string) {
        const raw = String(value ?? '');
        const { url: finalUrl, didRewrite } = maybeRewrite(raw);
        if (didRewrite && !alreadyRewritten(this, finalUrl)) {
          log.info(`${LOG_PREFIX}: intercepted instance src setter`, {
            original: raw,
            rewritten: finalUrl,
          });
          rewritten.set(this, finalUrl);
          applySrc(this, finalUrl);
        } else {
          applySrc(this, raw);
        }
      },
    });
  } catch {
    // Instance-level defineProperty can fail in some environments.
  }
}

function installSetAttributePatch(): void {
  if (typeof HTMLScriptElement === 'undefined') return;

  nativeSetAttribute = HTMLScriptElement.prototype.setAttribute;

  HTMLScriptElement.prototype.setAttribute = function patchedSetAttribute(
    this: HTMLScriptElement,
    name: string,
    value: string
  ): void {
    if (typeof name === 'string' && name.toLowerCase() === 'src') {
      const raw = String(value ?? '');
      const { url: finalUrl, didRewrite } = maybeRewrite(raw);
      if (didRewrite && !alreadyRewritten(this, finalUrl)) {
        log.info(`${LOG_PREFIX}: intercepted setAttribute('src')`, {
          original: raw,
          rewritten: finalUrl,
        });
        rewritten.set(this, finalUrl);
        nativeSetAttribute!.call(this, name, finalUrl);
        return;
      }
    }
    nativeSetAttribute!.call(this, name, value);
  };

  log.info(`${LOG_PREFIX}: setAttribute patch installed`);
}

// ---------------------------------------------------------------------------
// Layer 4: document.createElement patch
// ---------------------------------------------------------------------------

/**
 * Patch `document.createElement` so that every newly created `<script>`
 * element gets a per-instance `src` descriptor. This ensures coverage
 * even if the prototype-level descriptor failed to install.
 */
function installCreateElementPatch(): void {
  if (typeof document === 'undefined') return;

  nativeCreateElement = document.createElement;

  document.createElement = function patchedCreateElement(
    this: Document,
    tagName: string,
    options?: ElementCreationOptions
  ): HTMLElement {
    const el = nativeCreateElement!.call(this, tagName, options);
    if (typeof tagName === 'string' && tagName.toLowerCase() === 'script') {
      ensureInstancePatched(el as HTMLScriptElement);
    }
    return el;
  } as typeof document.createElement;

  log.info(`${LOG_PREFIX}: document.createElement patch installed`);
}

// ---------------------------------------------------------------------------
// Layer 5: DOM insertion patches (appendChild / insertBefore)
// ---------------------------------------------------------------------------

/**
 * Check a node at insertion time and rewrite if it's a GPT script or
 * preload link.
 */
function checkNodeAtInsertion(node: Node): void {
  if (!(node instanceof HTMLElement)) return;

  if (node.tagName === 'SCRIPT') {
    const src = (node as HTMLScriptElement).src || node.getAttribute('src') || '';
    if (src) rewriteScriptSrc(node as HTMLScriptElement, src);
  } else if (node.tagName === 'LINK') {
    rewriteLinkHref(node as HTMLLinkElement);
  }
}

function installDomInsertionPatches(): void {
  if (typeof Element === 'undefined') return;

  nativeAppendChild = Element.prototype.appendChild;
  nativeInsertBefore = Element.prototype.insertBefore;

  Element.prototype.appendChild = function <T extends Node>(this: Element, node: T): T {
    checkNodeAtInsertion(node);
    return nativeAppendChild!.call(this, node) as T;
  };

  Element.prototype.insertBefore = function <T extends Node>(
    this: Element,
    node: T,
    reference: Node | null
  ): T {
    checkNodeAtInsertion(node);
    return nativeInsertBefore!.call(this, node, reference) as T;
  };

  log.info(`${LOG_PREFIX}: DOM insertion patches installed`);
}

// ---------------------------------------------------------------------------
// Layer 6: MutationObserver
// ---------------------------------------------------------------------------

function installMutationObserver(): void {
  if (typeof document === 'undefined' || typeof MutationObserver === 'undefined') return;

  mutationObserver?.disconnect();
  mutationObserver = new MutationObserver((records) => {
    for (const record of records) {
      // Attribute mutation on an existing script element.
      if (record.type === 'attributes' && record.attributeName === 'src') {
        const target = record.target;
        if (target instanceof HTMLScriptElement) {
          const src = target.src || target.getAttribute('src') || '';
          if (src) rewriteScriptSrc(target, src);
        }
        continue;
      }

      // New nodes added to the DOM (catches innerHTML, document.write, .append(), etc.)
      if (record.type === 'childList') {
        record.addedNodes.forEach((node) => {
          if (node instanceof HTMLScriptElement) {
            const src = node.src || node.getAttribute('src') || '';
            if (src) rewriteScriptSrc(node, src);
            return;
          }
          if (node instanceof HTMLLinkElement) {
            rewriteLinkHref(node);
            return;
          }
          // Check children of container nodes (e.g. innerHTML sets a subtree).
          if (node instanceof Element) {
            node.querySelectorAll<HTMLScriptElement>('script[src]').forEach((script) => {
              const src = script.src || script.getAttribute('src') || '';
              if (src) rewriteScriptSrc(script, src);
            });
            node
              .querySelectorAll<HTMLLinkElement>('link[rel="preload"][as="script"]')
              .forEach((link) => rewriteLinkHref(link));
          }
        });
      }
    }
  });

  mutationObserver.observe(document, {
    subtree: true,
    childList: true,
    attributes: true,
    attributeFilter: ['src'],
  });

  log.info(`${LOG_PREFIX}: mutation observer active`);
}

// ---------------------------------------------------------------------------
// Guard lifecycle
// ---------------------------------------------------------------------------

let installed = false;

/**
 * Install the GPT guard.
 *
 * Sets up six interception layers (document.write, property descriptor,
 * setAttribute, createElement, DOM insertion, MutationObserver) to catch
 * GPT script URLs regardless of how they are set or inserted.
 */
export function installGptGuard(): void {
  if (installed) {
    log.debug(`${LOG_PREFIX}: already installed, skipping`);
    return;
  }

  if (typeof window === 'undefined') {
    log.debug(`${LOG_PREFIX}: not in browser environment, skipping`);
    return;
  }

  log.info(`${LOG_PREFIX}: installing interception for Google ad scripts`);

  // Layer 1: intercept document.write('<script src="...">') — GPT's primary path
  installDocumentWritePatch();

  // Layer 2: intercept .src = "..." on any HTMLScriptElement
  installSrcDescriptor();

  // Layer 3: intercept .setAttribute('src', "...")
  installSetAttributePatch();

  // Layer 4: intercept document.createElement('script') for instance-level patching
  installCreateElementPatch();

  // Layer 5: intercept appendChild / insertBefore (scripts + link preloads)
  installDomInsertionPatches();

  // Layer 6: catch anything else via MutationObserver
  installMutationObserver();

  installed = true;
  log.info(`${LOG_PREFIX}: all interception layers installed`);
}

/**
 * Check if the guard is currently installed.
 */
export function isGuardInstalled(): boolean {
  return installed;
}

/**
 * Reset the guard installation state (primarily for testing).
 */
export function resetGuardState(): void {
  mutationObserver?.disconnect();
  mutationObserver = undefined;

  if (typeof document !== 'undefined') {
    if (nativeDocWrite) {
      document.write = nativeDocWrite;
    }
    if (nativeDocWriteln) {
      document.writeln = nativeDocWriteln;
    }
    if (nativeCreateElement) {
      document.createElement = nativeCreateElement;
    }
  }

  if (typeof HTMLScriptElement !== 'undefined') {
    if (nativeSetAttribute) {
      HTMLScriptElement.prototype.setAttribute = nativeSetAttribute;
    }
    if (nativeSrcDescriptor) {
      try {
        Object.defineProperty(HTMLScriptElement.prototype, 'src', nativeSrcDescriptor);
      } catch {
        // Some test environments do not allow descriptor restoration.
      }
    }
  }

  if (typeof Element !== 'undefined') {
    if (nativeAppendChild) {
      Element.prototype.appendChild = nativeAppendChild;
    }
    if (nativeInsertBefore) {
      Element.prototype.insertBefore = nativeInsertBefore;
    }
  }

  nativeDocWrite = undefined;
  nativeDocWriteln = undefined;
  nativeSrcSet = undefined;
  nativeSrcGet = undefined;
  nativeSrcDescriptor = undefined;
  nativeSetAttribute = undefined;
  nativeCreateElement = undefined;
  nativeAppendChild = undefined;
  nativeInsertBefore = undefined;

  rewritten = new WeakMap<HTMLScriptElement | HTMLLinkElement, string>();
  instancePatched = new WeakSet<HTMLScriptElement>();

  installed = false;
}
