// Rendering utilities for Trusted Server demo placements: find slots, seed placeholders,
// and inject creatives into sandboxed iframes.
import createDOMPurify, {
  type DOMPurify as DOMPurifyInstance,
  type RemovedAttribute,
  type RemovedElement,
} from 'dompurify';

import { log } from './log';
import type { AdUnit } from './types';
import { getUnit, getAllUnits, firstSize } from './registry';
import NORMALIZE_CSS from './styles/normalize.css?inline';
import IFRAME_TEMPLATE from './templates/iframe.html?raw';

const DANGEROUS_TAG_NAMES = new Set([
  'base',
  'embed',
  'form',
  'iframe',
  'link',
  'meta',
  'object',
  'script',
]);
const URI_ATTRIBUTE_NAMES = new Set([
  'action',
  'background',
  'formaction',
  'href',
  'poster',
  'src',
  'srcdoc',
  'xlink:href',
]);
const DANGEROUS_URI_VALUE_PATTERN = /^\s*(?:javascript:|vbscript:|data\s*:\s*text\/html\b)/i;
const DANGEROUS_STYLE_PATTERN = /\bexpression\s*\(|\burl\s*\(\s*['"]?\s*javascript:/i;
const CREATIVE_SANDBOX_TOKENS = [
  'allow-forms',
  'allow-popups',
  'allow-popups-to-escape-sandbox',
  'allow-top-navigation-by-user-activation',
] as const;

export type CreativeSanitizationRejectionReason =
  | 'empty-after-sanitize'
  | 'invalid-creative-html'
  | 'removed-dangerous-content'
  | 'sanitizer-unavailable';

export type AcceptedCreativeHtml = {
  kind: 'accepted';
  originalLength: number;
  sanitizedHtml: string;
  sanitizedLength: number;
  removedCount: number;
};

export type RejectedCreativeHtml = {
  kind: 'rejected';
  originalLength: number;
  sanitizedLength: number;
  removedCount: number;
  rejectionReason: CreativeSanitizationRejectionReason;
};

export type SanitizeCreativeHtmlResult = AcceptedCreativeHtml | RejectedCreativeHtml;

let creativeSanitizer: DOMPurifyInstance | null | undefined;

function normalizeId(raw: string): string {
  const s = String(raw ?? '').trim();
  return s.startsWith('#') ? s.slice(1) : s;
}

function getCreativeSanitizer(): DOMPurifyInstance | null {
  if (creativeSanitizer !== undefined) {
    return creativeSanitizer;
  }

  if (typeof window === 'undefined') {
    creativeSanitizer = null;
    return creativeSanitizer;
  }

  try {
    creativeSanitizer = createDOMPurify(window);
  } catch (err) {
    log.warn('sanitizeCreativeHtml: failed to initialize DOMPurify', err);
    creativeSanitizer = null;
  }

  return creativeSanitizer;
}

function isDangerousRemoval(removedItem: RemovedAttribute | RemovedElement): boolean {
  if ('element' in removedItem) {
    const tagName = removedItem.element.nodeName.toLowerCase();
    return DANGEROUS_TAG_NAMES.has(tagName);
  }

  const attrName = removedItem.attribute?.name.toLowerCase() ?? '';
  const attrValue = removedItem.attribute?.value ?? '';

  if (attrName.startsWith('on')) {
    return true;
  }

  if (URI_ATTRIBUTE_NAMES.has(attrName)) {
    return true;
  }

  if (attrName === 'style' && DANGEROUS_STYLE_PATTERN.test(attrValue)) {
    return true;
  }

  return false;
}

function hasDangerousMarkup(candidateHtml: string): boolean {
  const fragment = document.createElement('template');
  // The HTML parser normalizes entity-encoded attribute values before we inspect them.
  fragment.innerHTML = candidateHtml;

  for (const element of fragment.content.querySelectorAll('*')) {
    const tagName = element.nodeName.toLowerCase();
    if (DANGEROUS_TAG_NAMES.has(tagName)) {
      return true;
    }

    if (tagName === 'style' && DANGEROUS_STYLE_PATTERN.test(element.textContent ?? '')) {
      return true;
    }

    for (const attrName of element.getAttributeNames()) {
      const normalizedAttrName = attrName.toLowerCase();
      const attrValue = element.getAttribute(attrName) ?? '';

      if (normalizedAttrName.startsWith('on')) {
        return true;
      }

      if (
        URI_ATTRIBUTE_NAMES.has(normalizedAttrName) &&
        DANGEROUS_URI_VALUE_PATTERN.test(attrValue)
      ) {
        return true;
      }

      if (normalizedAttrName === 'style' && DANGEROUS_STYLE_PATTERN.test(attrValue)) {
        return true;
      }
    }
  }

  return false;
}

// Sanitize the untrusted creative fragment before it is embedded into the trusted iframe shell.
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
  const sanitizer = getCreativeSanitizer();

  if (!sanitizer || !sanitizer.isSupported) {
    return {
      kind: 'rejected',
      originalLength,
      sanitizedLength: 0,
      removedCount: 0,
      rejectionReason: 'sanitizer-unavailable',
    };
  }

  const sanitizedHtml = sanitizer.sanitize(creativeHtml, {
    // Keep the result as a plain string because iframe.srcdoc expects string HTML.
    RETURN_TRUSTED_TYPE: false,
  });
  const removedItems = [...sanitizer.removed];
  const sanitizedLength = sanitizedHtml.length;

  if (removedItems.some(isDangerousRemoval) || hasDangerousMarkup(sanitizedHtml)) {
    return {
      kind: 'rejected',
      originalLength,
      sanitizedLength,
      removedCount: removedItems.length,
      rejectionReason: 'removed-dangerous-content',
    };
  }

  if (sanitizedHtml.trim().length === 0) {
    return {
      kind: 'rejected',
      originalLength,
      sanitizedLength,
      removedCount: removedItems.length,
      rejectionReason: 'empty-after-sanitize',
    };
  }

  return {
    kind: 'accepted',
    originalLength,
    sanitizedHtml,
    sanitizedLength,
    removedCount: removedItems.length,
  };
}

// Locate an ad slot element by id, tolerating funky selectors provided by tag managers.
export function findSlot(id: string): HTMLElement | null {
  const nid = normalizeId(id);
  // Fast path
  const byId = document.getElementById(nid) as HTMLElement | null;
  if (byId) return byId;
  // Fallback for odd IDs (special chars) or if provided with quotes/etc.
  try {
    const selector = `[id="${nid.replace(/"/g, '\\"')}"]`;
    const byAttr = document.querySelector(selector) as HTMLElement | null;
    if (byAttr) return byAttr;
  } catch {
    // Ignore selector errors (e.g., invalid characters)
  }
  return null;
}

function ensureSlot(id: string): HTMLElement {
  const nid = normalizeId(id);
  let el = document.getElementById(nid) as HTMLElement | null;
  if (el) return el;
  el = document.createElement('div');
  el.id = nid;
  const body: HTMLElement | null = typeof document !== 'undefined' ? document.body : null;
  if (body && typeof body.appendChild === 'function') {
    body.appendChild(el);
  } else {
    // DOM not ready — attach once available
    const element = el;
    const onReady = () => {
      const readyBody = document.body;
      if (readyBody && !document.getElementById(nid) && element) readyBody.appendChild(element);
    };
    document.addEventListener('DOMContentLoaded', onReady, { once: true });
  }
  return el;
}

// Drop a placeholder message into the slot so pages don't sit empty pre-render.
export function renderAdUnit(codeOrUnit: string | AdUnit): void {
  const code = typeof codeOrUnit === 'string' ? codeOrUnit : codeOrUnit?.code;
  if (!code) return;
  const unit = typeof codeOrUnit === 'string' ? getUnit(code) : codeOrUnit;
  const size = (unit && firstSize(unit)) || [300, 250];
  const el = ensureSlot(code);
  try {
    el.textContent = `Trusted Server — ${size[0]}x${size[1]}`;
    log.info('renderAdUnit: rendered placeholder', { code, size });
  } catch {
    log.warn('renderAdUnit: failed', { code });
  }
}

// Render placeholders for every registered ad unit (used in simple publisher demos).
export function renderAllAdUnits(): void {
  try {
    const parentReady =
      typeof document !== 'undefined' && (document.body || document.documentElement);
    if (!parentReady) {
      log.warn('renderAllAdUnits: DOM not ready; skipping');
      return;
    }
    const units = getAllUnits();
    for (const u of units) {
      renderAdUnit(u);
    }
    log.info('renderAllAdUnits: rendered all placeholders', { count: units.length });
  } catch (e) {
    log.warn('renderAllAdUnits: failed', e as unknown);
  }
}

type IframeOptions = { name?: string; title?: string; width?: number; height?: number };

// Construct a sandboxed iframe sized for sanitized, non-executable creative HTML.
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
  } catch (err) {
    log.debug('createAdIframe: sandbox add failed', err);
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

// Build a complete HTML document for a sanitized creative fragment, suitable for iframe.srcdoc.
export function buildCreativeDocument(creativeHtml: string): string {
  return IFRAME_TEMPLATE.replace('%NORMALIZE_CSS%', () => NORMALIZE_CSS).replace(
    '%CREATIVE_HTML%',
    () => creativeHtml
  );
}
