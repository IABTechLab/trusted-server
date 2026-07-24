// Rendering utilities for Trusted Server demo placements: find slots, seed placeholders,
// and inject creatives into sandboxed iframes.
import { log } from './log';
import type { AdUnit } from './types';
import { getUnit, getAllUnits, firstSize } from './registry';
import NORMALIZE_CSS from './styles/normalize.css?inline';
import IFRAME_TEMPLATE from './templates/iframe.html?raw';

// Sandbox permissions granted to creative iframes.
//
// Ad creatives routinely contain scripts for tracking, click handling, and
// viewability measurement, so `allow-scripts` is required for them to render.
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

export type CreativeSanitizationRejectionReason = 'empty-after-sanitize' | 'invalid-creative-html';

export type AcceptedCreativeHtml = {
  kind: 'accepted';
  originalLength: number;
  sanitizedHtml: string;
  // Always equal to originalLength: the client validates type/emptiness only;
  // server-side sanitization has already run before adm reaches this function.
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

function normalizeId(raw: string): string {
  const s = String(raw ?? '').trim();
  return s.startsWith('#') ? s.slice(1) : s;
}

// Validate the untrusted creative fragment before embedding it in the sandboxed iframe.
// Dangerous markup is stripped server-side before adm reaches the client; this function
// only guards against type errors and empty payloads. As a result, sanitizedLength always
// equals originalLength and removedCount is always 0 for accepted creatives — these fields
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
