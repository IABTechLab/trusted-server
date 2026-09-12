import fs from 'node:fs';
import path from 'node:path';

/** Discover the canonical integration bundle inventory used by build and runtime tests. */
export function discoverIntegrationModules(integrationsDirectory) {
  if (!fs.existsSync(integrationsDirectory)) return [];
  return fs
    .readdirSync(integrationsDirectory)
    .filter((name) => {
      const fullPath = path.join(integrationsDirectory, name);
      return fs.statSync(fullPath).isDirectory() && fs.existsSync(path.join(fullPath, 'index.ts'));
    })
    .sort();
}
