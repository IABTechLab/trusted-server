import path from 'node:path';
import { fileURLToPath } from 'node:url';

const library = path.dirname(fileURLToPath(import.meta.url));
const sibling = path.resolve(library, '../../trusted-server-integrations-js/lib');

/** Resolve sibling bare imports through Vite's normal exports-aware project resolver. */
export function browserPackageResolution() {
  return {
    name: 'canonical-browser-packages',
    enforce: 'pre',
    async resolveId(source, importer, options) {
      if (!importer?.startsWith(`${sibling}${path.sep}`) || /^(?:\.|\/|\0)/.test(source)) {
        return null;
      }
      return this.resolve(source, path.join(library, 'package.json'), {
        ...options,
        skipSelf: true,
      });
    },
  };
}
