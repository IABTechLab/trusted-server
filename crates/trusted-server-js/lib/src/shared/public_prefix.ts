// The deployment's public prefix, which every cookie name this script writes
// starts with, such as `ts` for `ts-eids`.
//
// The server writes it on the bundle's own tag, so the script holds no copy of
// its own. A page whose bundle tag carries none has no names to write, and a
// module asking for one is told `null` and writes nothing.

/** The `id` the server gives the bundle's `<script>` tag. */
const BUNDLE_TAG_ID = 'trustedserver-js';

/** The bundle tag attribute that carries the public prefix. */
export const PUBLIC_PREFIX_ATTRIBUTE = 'data-ts-public-prefix';

// Letters, digits, `_`, `-` and `.`, starting with a letter or a single `_`,
// which every prefix the server can write is.
const PREFIX_SHAPE = /^(?!__)[A-Za-z_][A-Za-z0-9._-]*$/;

/** The public prefix for this page, or `null` where the page carries none. */
export function publicPrefix(): string | null {
  try {
    if (typeof document === 'undefined') return null;
    const stamped = document.getElementById(BUNDLE_TAG_ID)?.getAttribute(PUBLIC_PREFIX_ATTRIBUTE);
    return typeof stamped === 'string' && PREFIX_SHAPE.test(stamped) ? stamped : null;
  } catch {
    return null;
  }
}

/**
 * The name of the cookie `name`, written `<prefix>-<name>`, such as `ts-eids`,
 * or `null` where the page carries no prefix.
 */
export function cookieName(name: string): string | null {
  const prefix = publicPrefix();
  return prefix ? `${prefix}-${name}` : null;
}
