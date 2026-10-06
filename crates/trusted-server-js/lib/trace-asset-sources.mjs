import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';

/** Hashes the explicit trace build inputs to reject stale assets when building is skipped. */
export function traceSourceDigest(library) {
  const sources = ['build-all.mjs', 'trace-asset-sources.mjs', 'package-lock.json'];
  function discover(directory) {
    for (const entry of fs.readdirSync(path.join(library, directory), { withFileTypes: true })) {
      const relative = `${directory}/${entry.name}`;
      if (entry.isDirectory()) discover(relative);
      else if (entry.isFile() && /\.(?:ts|css)$/.test(entry.name)) sources.push(relative);
    }
  }
  discover('src/trace');
  const hash = createHash('sha256');
  for (const relative of sources.sort((left, right) =>
    Buffer.compare(Buffer.from(left), Buffer.from(right))
  )) {
    hash
      .update(relative)
      .update('\0')
      .update(fs.readFileSync(path.join(library, relative)))
      .update('\0');
  }
  return hash.digest('hex');
}
