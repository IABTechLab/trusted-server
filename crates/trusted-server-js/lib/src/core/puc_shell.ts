const shellDocument = typeof document === 'undefined' ? undefined : document;
const shellWindow = shellDocument?.defaultView ?? undefined;
const queryAll = typeof Document === 'undefined' ? undefined : Document.prototype.querySelectorAll;
const connected =
  typeof Node === 'undefined'
    ? undefined
    : Object.getOwnPropertyDescriptor(Node.prototype, 'isConnected')?.get;
const parentElement =
  typeof Node === 'undefined'
    ? undefined
    : Object.getOwnPropertyDescriptor(Node.prototype, 'parentElement')?.get;
const frameWindow =
  typeof HTMLIFrameElement === 'undefined'
    ? undefined
    : Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'contentWindow')?.get;
const getAttribute = typeof Element === 'undefined' ? undefined : Element.prototype.getAttribute;
const closest = typeof Element === 'undefined' ? undefined : Element.prototype.closest;
const style =
  typeof HTMLElement === 'undefined'
    ? undefined
    : Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'style')?.get;
const setProperty =
  typeof CSSStyleDeclaration === 'undefined'
    ? undefined
    : CSSStyleDeclaration.prototype.setProperty;
const getComputedStyle = shellWindow?.getComputedStyle;

export interface CollapsedPucShellResizeInput {
  readonly source: object;
  readonly width: number;
  readonly height: number;
}

function attribute(element: Element, name: string): string | null | undefined {
  if (!getAttribute) return undefined;
  try {
    return Reflect.apply(getAttribute, element, [name]) as string | null;
  } catch {
    return undefined;
  }
}

function onePixelAttribute(element: Element, name: 'width' | 'height'): boolean {
  const value = attribute(element, name);
  if (value === undefined || value === null || !/^\d+(?:\.\d+)?$/.test(value)) return false;
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed <= 1;
}

function computedStyle(element: Element): CSSStyleDeclaration | undefined {
  if (!shellWindow || !getComputedStyle) return undefined;
  try {
    return Reflect.apply(getComputedStyle, shellWindow, [element]) as CSSStyleDeclaration;
  } catch {
    return undefined;
  }
}

function onePixelComputed(value: CSSStyleDeclaration, dimension: 'width' | 'height'): boolean {
  const match = /^(\d+(?:\.\d+)?)px$/.exec(dimension === 'width' ? value.width : value.height);
  return match !== null && Number(match[1]) <= 1;
}

function ordinaryCollapsedElement(element: HTMLElement): boolean {
  const value = computedStyle(element);
  return (
    value !== undefined &&
    value.position !== 'fixed' &&
    value.position !== 'sticky' &&
    onePixelComputed(value, 'width') &&
    onePixelComputed(value, 'height')
  );
}

/** Resize only the authenticated, ordinary collapsed Universal Creative shell. */
export function resizeCollapsedPucShell(input: CollapsedPucShellResizeInput): boolean {
  if (
    !shellDocument ||
    !queryAll ||
    !connected ||
    !parentElement ||
    !frameWindow ||
    !closest ||
    !style ||
    !setProperty ||
    typeof input !== 'object' ||
    input === null ||
    typeof input.source !== 'object' ||
    input.source === null ||
    !Number.isFinite(input.width) ||
    !Number.isFinite(input.height) ||
    input.width <= 0 ||
    input.height <= 0
  ) {
    return false;
  }

  try {
    const candidates = Reflect.apply(queryAll, shellDocument, [
      'iframe',
    ]) as NodeListOf<HTMLIFrameElement>;
    let frame: HTMLIFrameElement | undefined;
    for (let index = 0; index < candidates.length; index += 1) {
      const candidate = candidates.item(index);
      if (
        candidate &&
        Reflect.apply(connected, candidate, []) === true &&
        Reflect.apply(frameWindow, candidate, []) === input.source
      ) {
        if (frame) return false;
        frame = candidate;
      }
    }
    if (
      !frame ||
      !onePixelAttribute(frame, 'width') ||
      !onePixelAttribute(frame, 'height') ||
      !ordinaryCollapsedElement(frame) ||
      Reflect.apply(closest, frame, ['a,[data-anchor-status]']) !== null
    ) {
      return false;
    }

    const wrapper = Reflect.apply(parentElement, frame, []) as HTMLElement | null;
    if (
      !wrapper ||
      wrapper === shellDocument.body ||
      wrapper === shellDocument.documentElement ||
      wrapper instanceof HTMLAnchorElement ||
      Reflect.apply(connected, wrapper, []) !== true ||
      !ordinaryCollapsedElement(wrapper) ||
      Reflect.apply(closest, wrapper, ['a,[data-anchor-status]']) !== null
    ) {
      return false;
    }

    const frameStyle = Reflect.apply(style, frame, []) as CSSStyleDeclaration;
    const wrapperStyle = Reflect.apply(style, wrapper, []) as CSSStyleDeclaration;
    Reflect.apply(setProperty, frameStyle, ['width', `${input.width}px`]);
    Reflect.apply(setProperty, frameStyle, ['height', `${input.height}px`]);
    Reflect.apply(setProperty, wrapperStyle, ['width', `${input.width}px`]);
    Reflect.apply(setProperty, wrapperStyle, ['height', `${input.height}px`]);
    return true;
  } catch {
    return false;
  }
}
