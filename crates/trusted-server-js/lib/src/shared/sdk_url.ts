/**
 * Parse an HTTP(S) SDK URL with an explicit authority, or a protocol-relative URL.
 *
 * Uses the same C0/space, tab/CR/LF, and authority policy as Rust's SDK URL parser.
 * Literal NULs, relative paths, and base-dependent shorthand stay unowned.
 * Browser/Rust ownership cases live in test/fixtures/sdk-url-ownership.json.
 */
export function parseSdkNetworkUrl(url: string): URL | null {
  // HTML replaces NULs rather than treating them as URL whitespace.
  if (url.includes('\0')) return null;
  // eslint-disable-next-line no-control-regex -- Match WHATWG C0 control and space trimming.
  const value = url.replace(/^[\u0000-\u0020]+|[\u0000-\u0020]+$/g, '').replace(/[\t\n\r]/g, '');
  if (!/^(?:https?:)?[/\\]{2}/i.test(value)) return null;

  try {
    return new URL(/^[/\\]{2}/.test(value) ? `https:${value}` : value);
  } catch {
    return null;
  }
}
