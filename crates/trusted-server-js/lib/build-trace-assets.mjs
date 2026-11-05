import { createHash } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { buildTraceAssets } from './build-trace.mjs';
import { traceSourceDigest } from './trace-asset-sources.mjs';

const library = path.dirname(fileURLToPath(import.meta.url));
// v1 is still unpublished. Once released, add a version rather than replacing its bytes.
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'trace-assets-build-'));
try {
  await buildTraceAssets(temporary);
  const contents = ['v1.js', 'v1.css'].map((file) => ({
    file,
    bytes: fs.readFileSync(path.join(temporary, 'trace', file)),
  }));
  const manifest = {
    schema_version: 1,
    source_sha256: traceSourceDigest(library),
    assets: contents.map(({ file, bytes }) => ({
      path: `/_ts/trace/assets/${file}`,
      file,
      sha256: createHash('sha256').update(bytes).digest('hex'),
    })),
  };
  fs.mkdirSync(path.join(library, 'trace-assets'), { recursive: true });
  for (const { file, bytes } of contents)
    fs.writeFileSync(path.join(library, 'trace-assets', file), bytes);
  fs.writeFileSync(
    path.join(library, 'trace-assets-manifest.json'),
    JSON.stringify(manifest, null, 2) + '\n'
  );
} finally {
  fs.rmSync(temporary, { recursive: true, force: true });
}
