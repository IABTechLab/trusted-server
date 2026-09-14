function realmOwnedInstance(
  candidate: unknown,
  targetRealm: unknown,
  constructorName: 'Document' | 'Element' | 'HTMLElement'
): object | undefined {
  try {
    if (typeof candidate !== 'object' || candidate === null || Array.isArray(candidate)) {
      return undefined;
    }
    if (
      (typeof targetRealm !== 'object' && typeof targetRealm !== 'function') ||
      targetRealm === null
    ) {
      return undefined;
    }
    const constructor = (targetRealm as Readonly<Record<string, unknown>>)[constructorName];
    return typeof constructor === 'function' && candidate instanceof constructor
      ? candidate
      : undefined;
  } catch {
    return undefined;
  }
}

/** Return a Document only when its own browsing-context realm authenticates it. */
export function realmOwnedDocument(candidate: unknown): Document | undefined {
  try {
    const targetWindow = (candidate as { readonly defaultView?: unknown } | null)?.defaultView;
    const document = realmOwnedInstance(candidate, targetWindow, 'Document');
    if (!document) return undefined;
    const realmDocument = (targetWindow as { readonly document?: unknown }).document;
    return realmDocument === candidate ? (document as Document) : undefined;
  } catch {
    return undefined;
  }
}

/** Return an Element only when the supplied target realm authenticates it. */
export function realmOwnedElement(candidate: unknown, targetRealm: unknown): Element | undefined {
  return realmOwnedInstance(candidate, targetRealm, 'Element') as Element | undefined;
}

/** Return an HTMLElement only when the supplied target realm authenticates it. */
export function realmOwnedHtmlElement(
  candidate: unknown,
  targetRealm: unknown
): HTMLElement | undefined {
  return realmOwnedInstance(candidate, targetRealm, 'HTMLElement') as HTMLElement | undefined;
}
