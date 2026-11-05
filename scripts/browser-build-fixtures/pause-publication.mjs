import fs from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";

// Pause after the real scope has reported its identity, immediately before
// complete metadata publication. The scope must still be behind its tool gate.
const root = process.env.LEASE_FIXTURE_PUBLICATION_ROOT;
const rename = fs.promises.rename;
fs.promises.rename = async function (from, to) {
  if (root && from === path.join(root, ".browser-build-lease/owner.pending")) {
    fs.writeFileSync(path.join(root, "publishing"), "");
    while (!fs.existsSync(path.join(root, "publish"))) await delay(10);
  }
  return rename(from, to);
};
syncBuiltinESMExports();
