/**
 * Parse an HTTP(S) SDK URL with an explicit authority, or a protocol-relative URL.
 *
 * Uses the same ASCII whitespace and prefix policy as Rust's SDK URL parser.
 * Browser/Rust ownership cases live in test/fixtures/sdk-url-ownership.json.
 */
export function parseSdkNetworkUrl(url: string): URL | null {
  const value = url.replace(/^[ \t\n\r\f\v]+|[ \t\n\r\f\v]+$/g, '');
  if (!/^(?:https?:)?\/\//i.test(value)) return null;

  try {
    return new URL(value.startsWith('//') ? `https:${value}` : value);
  } catch {
    return null;
  }
}
