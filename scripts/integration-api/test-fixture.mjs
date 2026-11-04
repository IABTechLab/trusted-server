import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { after } from "node:test";

const scratch = mkdtempSync(path.join(tmpdir(), "integration-api-"));
after(() => rmSync(scratch, { recursive: true, force: true }));
export const source = readFileSync(
  new URL("./fixture.rs", import.meta.url),
  "utf8",
);
let sequence = 0;

export function withAddedFunction(document, name) {
  const changed = structuredClone(document);
  const root = changed.index[changed.root];
  const added = structuredClone(
    Object.values(changed.index).find((item) => item.name === "make"),
  );
  added.id = 999999991;
  added.name = name;
  changed.index[added.id] = added;
  changed.paths[added.id] = {
    crate_id: root.crate_id,
    path: [root.name, name],
    kind: "function",
  };
  root.inner.module.items.push(added.id);
  return changed;
}

export function compile(
  value = source,
  { privateItems = true, target, cfg } = {},
) {
  const directory = path.join(scratch, `case-${sequence++}`);
  mkdirSync(directory);
  const file = path.join(directory, "lib.rs");
  writeFileSync(file, value);
  const args = [
    "-Z",
    "unstable-options",
    "--output-format",
    "json",
    "--document-hidden-items",
    "--crate-name",
    "trusted_server_core",
    "--crate-type",
    "lib",
    "--edition",
    "2024",
    file,
    "-o",
    directory,
  ];
  if (privateItems) args.push("--document-private-items");
  if (target) args.push("--target", target);
  if (cfg) args.push("--cfg", cfg);
  const result = spawnSync("rustdoc", args, {
    encoding: "utf8",
    env: { ...process.env, RUSTC_BOOTSTRAP: "trusted_server_core" },
  });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(
    readFileSync(path.join(directory, "trusted_server_core.json")),
  );
}
