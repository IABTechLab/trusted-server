import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';

/** Hash trace sources/settings and only the resolved build-tool dependency graph. */
export function traceSourceDigest(library) {
  const definition = 'trace-build-inputs.json';
  const inputs = JSON.parse(fs.readFileSync(path.join(library, definition), 'utf8'));
  const sources = [definition, ...inputs.files];
  function discover(directory) {
    for (const entry of fs.readdirSync(path.join(library, directory), { withFileTypes: true })) {
      const relative = `${directory}/${entry.name}`;
      if (entry.isDirectory()) discover(relative);
      else if (entry.isFile()) sources.push(relative);
    }
  }
  inputs.source_roots.forEach(discover);
  const hash = createHash('sha256');
  const add = (name, value) => hash.update(name).update('\0').update(value).update('\0');
  for (const relative of sources.sort((left, right) =>
    Buffer.compare(Buffer.from(left), Buffer.from(right))
  ))
    add(relative, fs.readFileSync(path.join(library, relative)));

  const { packages } = JSON.parse(fs.readFileSync(path.join(library, 'package-lock.json'), 'utf8'));
  const selected = new Set();
  const pending = inputs.toolchain_roots.map((name) => `node_modules/${name}`);
  while (pending.length) {
    const location = pending.pop();
    if (selected.has(location)) continue;
    selected.add(location);
    const entry = packages[location];
    if (!entry) throw new Error(`missing locked trace build dependency: ${location}`);
    for (const name of Object.keys({ ...entry.dependencies, ...entry.optionalDependencies })) {
      let parent = location;
      let resolved;
      while (parent) {
        const candidate = `${parent}/node_modules/${name}`;
        if (packages[candidate]) {
          resolved = candidate;
          break;
        }
        const index = parent.lastIndexOf('/node_modules/');
        parent = index < 0 ? '' : parent.slice(0, index);
      }
      resolved ??= `node_modules/${name}`;
      if (!packages[resolved]) throw new Error(`missing locked trace build dependency: ${name}`);
      pending.push(resolved);
    }
  }
  for (const location of [...selected].sort()) {
    for (const key of ['version', 'resolved', 'integrity']) {
      add(`${location}/${key}`, packages[location][key] ?? '');
    }
  }
  return hash.digest('hex');
}
