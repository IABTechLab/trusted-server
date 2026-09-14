import { log } from '../core/log';

import {
  DEFAULT_DOM_INSERTION_HANDLER_PRIORITY,
  registerDomInsertionHandler,
  type DomInsertionCandidate,
} from './dom_insertion_dispatcher';

/** Maximum fallback instance descriptors retained until one guard is reset. */
const MAX_TRACKED_INSTANCE_PATCHES = 256;

/** Optional interception layers needed by SDKs that load child scripts themselves. */
export interface DeepScriptInterceptionConfig {
  /** Cheap fail-closed hint used before parsing HTML passed to document.write/writeln. */
  readonly documentWriteUrlHint: string;
}

interface ScriptGuardConfigBase {
  /** Install source setters, document-write parsing, and a mutation observer. */
  deepInterception?: DeepScriptInterceptionConfig;
  /** Integration ID used for deterministic dispatcher ordering. */
  id: string;
  /** Return true only for a URL owned by this integration. */
  isTargetUrl: (url: string) => boolean;
  /** Optional human-readable log label. */
  displayName?: string;
  /** Lower values run earlier when multiple insertion handlers match. */
  priority?: number;
}

interface ScriptGuardConfigWithProxyPath extends ScriptGuardConfigBase {
  proxyPath: string;
  rewriteUrl?: never;
}

interface ScriptGuardConfigWithRewriter extends ScriptGuardConfigBase {
  proxyPath?: never;
  rewriteUrl: (originalUrl: string) => string;
}

export type ScriptGuardConfig = ScriptGuardConfigWithProxyPath | ScriptGuardConfigWithRewriter;

export interface ScriptGuard {
  install: () => void;
  isInstalled: () => boolean;
  reset: () => void;
}

interface InstancePatch {
  readonly element: HTMLScriptElement;
  readonly previous: PropertyDescriptor | undefined;
  readonly setter: (this: HTMLScriptElement, value: string) => void;
}

function rewriteToFirstParty(proxyPath: string): string {
  return `${window.location.origin}${proxyPath}`;
}

function rewrittenUrl(originalUrl: string, config: ScriptGuardConfig): string {
  return config.rewriteUrl ? config.rewriteUrl(originalUrl) : rewriteToFirstParty(config.proxyPath);
}

/**
 * Create one reversible guard. Basic guards share only the insertion dispatcher;
 * deep guards additionally own the browser patches required by self-loading SDKs.
 */
export function createScriptGuard(config: ScriptGuardConfig): ScriptGuard {
  const prefix = `${config.displayName ?? config.id} guard`;
  let installed = false;
  let unregister: (() => void) | undefined;
  let mutationObserver: MutationObserver | undefined;
  let nativeDocWrite: typeof document.write | undefined;
  let nativeDocWriteln: typeof document.writeln | undefined;
  let documentWriteWrapper: typeof document.write | undefined;
  let documentWritelnWrapper: typeof document.writeln | undefined;
  let nativeCreateElement: typeof document.createElement | undefined;
  let createElementWrapper: typeof document.createElement | undefined;
  let nativeSetAttribute: typeof HTMLScriptElement.prototype.setAttribute | undefined;
  let setAttributeWrapper: typeof HTMLScriptElement.prototype.setAttribute | undefined;
  let nativeSrcDescriptor: PropertyDescriptor | undefined;
  let nativeSrcGet: ((this: HTMLScriptElement) => string) | undefined;
  let nativeSrcSet: ((this: HTMLScriptElement, value: string) => void) | undefined;
  let installedSrcSetter: ((this: HTMLScriptElement, value: string) => void) | undefined;
  let srcDescriptorInstalled = false;
  let rewritten = new WeakMap<HTMLScriptElement | HTMLLinkElement, string>();
  let patchedInstances = new Map<HTMLScriptElement, InstancePatch>();

  const isTarget = (url: string): boolean => {
    try {
      return config.isTargetUrl(url);
    } catch {
      return false;
    }
  };

  const rewrite = (url: string): string => {
    try {
      return rewrittenUrl(url, config);
    } catch {
      return url;
    }
  };

  const applyScriptSource = (element: HTMLScriptElement, value: string): void => {
    if (nativeSrcSet) {
      nativeSrcSet.call(element, value);
      return;
    }
    if (nativeSetAttribute) {
      nativeSetAttribute.call(element, 'src', value);
      return;
    }
    element.setAttribute('src', value);
  };

  const rewriteScriptSource = (element: HTMLScriptElement, rawUrl: string): boolean => {
    if (!isTarget(rawUrl)) return false;
    const finalUrl = rewrite(rawUrl);
    if (finalUrl === rawUrl || rewritten.get(element) === finalUrl) return false;
    rewritten.set(element, finalUrl);
    log.info(`${prefix}: rewriting script src`, { original: rawUrl, rewritten: finalUrl });
    applyScriptSource(element, finalUrl);
    return true;
  };

  const rewriteLinkSource = (
    element: HTMLLinkElement,
    rawUrl: string,
    rel: string | null
  ): boolean => {
    if (
      (rel !== 'preload' && rel !== 'prefetch') ||
      element.getAttribute('as') !== 'script' ||
      !isTarget(rawUrl)
    ) {
      return false;
    }
    const finalUrl = rewrite(rawUrl);
    if (finalUrl === rawUrl || rewritten.get(element) === finalUrl) return false;
    rewritten.set(element, finalUrl);
    log.info(`${prefix}: rewriting SDK ${rel} link`, { original: rawUrl, rewritten: finalUrl });
    element.href = finalUrl;
    element.setAttribute('href', finalUrl);
    return true;
  };

  const rewriteCandidate = (candidate: DomInsertionCandidate): boolean => {
    if (!isTarget(candidate.url)) return false;
    if (candidate.kind === 'script') {
      if (config.deepInterception) return rewriteScriptSource(candidate.element, candidate.url);
      const finalUrl = rewrite(candidate.url);
      candidate.element.src = finalUrl;
      candidate.element.setAttribute('src', finalUrl);
      log.info(`${prefix}: rewriting dynamically inserted SDK script`, {
        framework: candidate.element.getAttribute('data-nscript') || 'generic',
        original: candidate.url,
        rewritten: finalUrl,
      });
      return true;
    }
    if (config.deepInterception) {
      return rewriteLinkSource(candidate.element, candidate.url, candidate.rel);
    }
    const finalUrl = rewrite(candidate.url);
    candidate.element.href = finalUrl;
    candidate.element.setAttribute('href', finalUrl);
    log.info(`${prefix}: rewriting SDK ${candidate.rel} link`, {
      as: candidate.element.getAttribute('as'),
      original: candidate.url,
      rel: candidate.rel,
      rewritten: finalUrl,
    });
    return true;
  };

  const rewriteDocumentHtml = (html: string): string => {
    const hint = config.deepInterception?.documentWriteUrlHint;
    if (!hint || !html.includes(hint)) return html;
    if (typeof DOMParser === 'undefined') {
      log.warn(`${prefix}: DOMParser unavailable, blocking matching document.write HTML`);
      return '';
    }
    try {
      const parsed = new DOMParser().parseFromString(html, 'text/html');
      const scripts = parsed.querySelectorAll('script[src]');
      let changed = false;
      for (let index = 0; index < scripts.length; index += 1) {
        const script = scripts.item(index);
        const rawUrl = script.getAttribute('src') ?? '';
        if (!isTarget(rawUrl)) continue;
        const finalUrl = rewrite(rawUrl);
        if (finalUrl === rawUrl) continue;
        script.setAttribute('src', finalUrl);
        changed = true;
      }
      return changed ? (parsed.head?.innerHTML ?? '') + (parsed.body?.innerHTML ?? '') : html;
    } catch (error) {
      log.warn(`${prefix}: failed to parse matching document.write HTML, blocking`, error);
      return '';
    }
  };

  const installDocumentWritePatch = (): void => {
    if (typeof document === 'undefined') return;
    nativeDocWrite = document.write;
    nativeDocWriteln = document.writeln;
    documentWriteWrapper = function (this: Document, ...args: string[]): void {
      nativeDocWrite?.apply(
        this,
        args.map((value) => (typeof value === 'string' ? rewriteDocumentHtml(value) : value))
      );
    };
    documentWritelnWrapper = function (this: Document, ...args: string[]): void {
      nativeDocWriteln?.apply(
        this,
        args.map((value) => (typeof value === 'string' ? rewriteDocumentHtml(value) : value))
      );
    };
    document.write = documentWriteWrapper;
    document.writeln = documentWritelnWrapper;
  };

  const installSrcDescriptor = (): void => {
    if (typeof HTMLScriptElement === 'undefined') return;
    const descriptor = Object.getOwnPropertyDescriptor(HTMLScriptElement.prototype, 'src');
    if (!descriptor || typeof descriptor.set !== 'function' || descriptor.configurable === false) {
      return;
    }
    nativeSrcDescriptor = descriptor;
    nativeSrcGet = typeof descriptor.get === 'function' ? descriptor.get : undefined;
    nativeSrcSet = descriptor.set;
    installedSrcSetter = function (this: HTMLScriptElement, value: string): void {
      const rawUrl = String(value ?? '');
      if (!rewriteScriptSource(this, rawUrl)) applyScriptSource(this, rawUrl);
    };
    try {
      Object.defineProperty(HTMLScriptElement.prototype, 'src', {
        configurable: true,
        enumerable: descriptor.enumerable ?? true,
        get(this: HTMLScriptElement): string {
          return nativeSrcGet ? nativeSrcGet.call(this) : (this.getAttribute('src') ?? '');
        },
        set: installedSrcSetter,
      });
      srcDescriptorInstalled = true;
    } catch {
      nativeSrcDescriptor = undefined;
      nativeSrcGet = undefined;
      nativeSrcSet = undefined;
      installedSrcSetter = undefined;
    }
  };

  const installSetAttributePatch = (): void => {
    if (typeof HTMLScriptElement === 'undefined') return;
    nativeSetAttribute = HTMLScriptElement.prototype.setAttribute;
    setAttributeWrapper = function (this: HTMLScriptElement, name: string, value: string): void {
      if (typeof name === 'string' && name.toLowerCase() === 'src') {
        const rawUrl = String(value ?? '');
        if (isTarget(rawUrl)) {
          const finalUrl = rewrite(rawUrl);
          if (finalUrl !== rawUrl && rewritten.get(this) !== finalUrl) {
            rewritten.set(this, finalUrl);
            nativeSetAttribute?.call(this, name, finalUrl);
            return;
          }
        }
      }
      nativeSetAttribute?.call(this, name, value);
    };
    HTMLScriptElement.prototype.setAttribute = setAttributeWrapper;
  };

  const ensureInstancePatched = (element: HTMLScriptElement): void => {
    if (srcDescriptorInstalled || patchedInstances.has(element)) return;
    if (patchedInstances.size >= MAX_TRACKED_INSTANCE_PATCHES) return;
    const previous = Object.getOwnPropertyDescriptor(element, 'src');
    const setter = function (this: HTMLScriptElement, value: string): void {
      const rawUrl = String(value ?? '');
      if (!rewriteScriptSource(this, rawUrl)) applyScriptSource(this, rawUrl);
    };
    try {
      Object.defineProperty(element, 'src', {
        configurable: true,
        enumerable: true,
        get(this: HTMLScriptElement): string {
          return nativeSrcGet ? nativeSrcGet.call(this) : (this.getAttribute('src') ?? '');
        },
        set: setter,
      });
      patchedInstances.set(element, { element, previous, setter });
    } catch {
      // The insertion dispatcher and observer remain fail-safe fallbacks.
    }
  };

  const installCreateElementPatch = (): void => {
    if (typeof document === 'undefined') return;
    nativeCreateElement = document.createElement;
    createElementWrapper = function (
      this: Document,
      tagName: string,
      options?: ElementCreationOptions
    ): HTMLElement {
      const element = nativeCreateElement!.call(this, tagName, options);
      if (typeof tagName === 'string' && tagName.toLowerCase() === 'script') {
        ensureInstancePatched(element as HTMLScriptElement);
      }
      return element;
    } as typeof document.createElement;
    document.createElement = createElementWrapper;
  };

  const inspectMutationNode = (node: Node): void => {
    if (node instanceof HTMLScriptElement) {
      const rawUrl = node.src || node.getAttribute('src') || '';
      if (rawUrl) rewriteScriptSource(node, rawUrl);
      return;
    }
    if (node instanceof HTMLLinkElement) {
      rewriteLinkSource(
        node,
        node.href || node.getAttribute('href') || '',
        node.getAttribute('rel')
      );
      return;
    }
    if (!(node instanceof Element)) return;
    const descendants = node.querySelectorAll<HTMLScriptElement | HTMLLinkElement>(
      'script[src],link[rel="preload"][as="script"],link[rel="prefetch"][as="script"]'
    );
    for (let index = 0; index < descendants.length; index += 1) {
      inspectMutationNode(descendants.item(index));
    }
  };

  const installMutationObserver = (): void => {
    if (typeof document === 'undefined' || typeof MutationObserver === 'undefined') return;
    mutationObserver = new MutationObserver((records) => {
      for (let recordIndex = 0; recordIndex < records.length; recordIndex += 1) {
        const record = records[recordIndex];
        if (!record) continue;
        if (record.type === 'attributes' && record.attributeName === 'src') {
          inspectMutationNode(record.target);
          continue;
        }
        if (record.type !== 'childList') continue;
        for (let nodeIndex = 0; nodeIndex < record.addedNodes.length; nodeIndex += 1) {
          const node = record.addedNodes.item(nodeIndex);
          if (node) inspectMutationNode(node);
        }
      }
    });
    mutationObserver.observe(document, {
      attributeFilter: ['src'],
      attributes: true,
      childList: true,
      subtree: true,
    });
  };

  const install = (): void => {
    if (installed) return;
    if (
      typeof window === 'undefined' ||
      (!config.deepInterception && typeof Element === 'undefined')
    ) {
      return;
    }
    if (config.deepInterception) {
      installDocumentWritePatch();
      installSrcDescriptor();
      installSetAttributePatch();
      installCreateElementPatch();
      installMutationObserver();
    }
    unregister = registerDomInsertionHandler({
      handle: rewriteCandidate,
      id: config.id,
      priority: config.priority ?? DEFAULT_DOM_INSERTION_HANDLER_PRIORITY,
    });
    installed = true;
  };

  const reset = (): void => {
    mutationObserver?.disconnect();
    mutationObserver = undefined;
    unregister?.();
    unregister = undefined;

    for (const patch of patchedInstances.values()) {
      try {
        const current = Object.getOwnPropertyDescriptor(patch.element, 'src');
        if (current?.set !== patch.setter) continue;
        if (patch.previous) Object.defineProperty(patch.element, 'src', patch.previous);
        else Reflect.deleteProperty(patch.element, 'src');
      } catch {
        // External replacement wins; do not overwrite it during cleanup.
      }
    }
    patchedInstances.clear();

    if (typeof document !== 'undefined') {
      if (document.write === documentWriteWrapper && nativeDocWrite)
        document.write = nativeDocWrite;
      if (document.writeln === documentWritelnWrapper && nativeDocWriteln) {
        document.writeln = nativeDocWriteln;
      }
      if (document.createElement === createElementWrapper && nativeCreateElement) {
        document.createElement = nativeCreateElement;
      }
    }
    if (typeof HTMLScriptElement !== 'undefined') {
      if (HTMLScriptElement.prototype.setAttribute === setAttributeWrapper && nativeSetAttribute) {
        HTMLScriptElement.prototype.setAttribute = nativeSetAttribute;
      }
      const current = Object.getOwnPropertyDescriptor(HTMLScriptElement.prototype, 'src');
      if (current?.set === installedSrcSetter && nativeSrcDescriptor) {
        try {
          Object.defineProperty(HTMLScriptElement.prototype, 'src', nativeSrcDescriptor);
        } catch {
          // External hardening can make restoration impossible; keep cleanup contained.
        }
      }
    }

    nativeDocWrite = undefined;
    nativeDocWriteln = undefined;
    documentWriteWrapper = undefined;
    documentWritelnWrapper = undefined;
    nativeCreateElement = undefined;
    createElementWrapper = undefined;
    nativeSetAttribute = undefined;
    setAttributeWrapper = undefined;
    nativeSrcDescriptor = undefined;
    nativeSrcGet = undefined;
    nativeSrcSet = undefined;
    installedSrcSetter = undefined;
    srcDescriptorInstalled = false;
    rewritten = new WeakMap();
    patchedInstances = new Map();
    installed = false;
  };

  return { install, isInstalled: () => installed, reset };
}
