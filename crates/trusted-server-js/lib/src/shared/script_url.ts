/** Parse a source reference against the browser's actual document base. */
export function parseScriptUrl(value: string): URL | undefined {
  if (!value?.trim()) return undefined;
  try {
    const parsed =
      typeof document === 'undefined' ? new URL(value) : new URL(value, document.baseURI);
    return parsed.protocol === 'http:' || parsed.protocol === 'https:' ? parsed : undefined;
  } catch {
    return undefined;
  }
}

/** Preserve the parsed path, query and fragment when proxying a source. */
export function scriptUrlPath(url: URL): string {
  return `${url.pathname}${url.search}${url.hash}`;
}
