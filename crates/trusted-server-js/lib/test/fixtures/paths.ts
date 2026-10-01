import { resolve } from 'node:path';

/**
 * Absolute path to the edge-injected `gpt_bootstrap.js` that several suites
 * evaluate verbatim.
 *
 * Resolved from `import.meta.dirname`, which Node defines as a platform-native
 * directory path. A `file:` URL pathname is not a filesystem path, because on
 * Windows it carries a leading slash before the drive letter, so reading it
 * directly makes Node resolve it against the current drive and the drive
 * segment doubles. Resolving from this module's own directory also keeps the
 * path independent of the working directory the suite is launched from.
 */
export const GPT_BOOTSTRAP_PATH = resolve(
  import.meta.dirname,
  '../../../../trusted-server-core/src/integrations/gpt_bootstrap.js'
);
