/** Result of resolving one configured slot div ID against the live DOM. */
export interface SlotElementResolution {
  element: HTMLElement | null;
  prefixMatchCount: number;
  activeMatchCount: number;
}

function isElementVisible(element: HTMLElement): boolean {
  const elementWithVisibilityCheck = element as HTMLElement & {
    checkVisibility?: (options?: {
      checkVisibilityCSS?: boolean;
      visibilityProperty?: boolean;
    }) => boolean;
  };
  if (typeof elementWithVisibilityCheck.checkVisibility === 'function') {
    return elementWithVisibilityCheck.checkVisibility({
      checkVisibilityCSS: true,
      visibilityProperty: true,
    });
  }

  for (let current: HTMLElement | null = element; current; current = current.parentElement) {
    const style = window.getComputedStyle(current);
    if (
      style.display === 'none' ||
      style.visibility === 'hidden' ||
      style.visibility === 'collapse'
    ) {
      return false;
    }
  }
  return true;
}

function slotElementHasLayout(element: HTMLElement): boolean {
  if (!isElementVisible(element)) return false;
  const elementRect = element.getBoundingClientRect();
  if (elementRect.width > 0 && elementRect.height > 0) return true;

  const container = document.getElementById(`${element.id}-container`);
  if (!container || !isElementVisible(container)) return false;
  const containerRect = container.getBoundingClientRect();
  return containerRect.width > 0;
}

/** Resolve an exact ID or one unambiguous visible/layout prefix match. */
export function resolveSlotElementByDivId(divId: string): SlotElementResolution {
  if (!divId) {
    return { element: null, prefixMatchCount: 0, activeMatchCount: 0 };
  }

  const exact = document.getElementById(divId);
  if (exact) {
    return { element: exact, prefixMatchCount: 1, activeMatchCount: 1 };
  }

  const prefixMatches = Array.from(document.querySelectorAll<HTMLElement>('[id]')).filter(
    (element) => element.id.startsWith(divId) && !element.id.endsWith('-container')
  );
  if (prefixMatches.length === 1 && isElementVisible(prefixMatches[0]!)) {
    return {
      element: prefixMatches[0]!,
      prefixMatchCount: 1,
      activeMatchCount: 1,
    };
  }

  const visibleMatches = prefixMatches.filter(isElementVisible);
  if (visibleMatches.length === 1) {
    return {
      element: visibleMatches[0]!,
      prefixMatchCount: prefixMatches.length,
      activeMatchCount: 1,
    };
  }

  const activeMatches = visibleMatches.filter(slotElementHasLayout);
  return {
    element: activeMatches.length === 1 ? activeMatches[0]! : null,
    prefixMatchCount: prefixMatches.length,
    activeMatchCount: activeMatches.length,
  };
}
