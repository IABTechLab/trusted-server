import fs from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";

// Produce a genuinely stale record only after the command group has drained
// and the holder has reaped its direct child. No zombie or scope proof is faked.
const root = process.env.LEASE_FIXTURE_RETIREMENT_ROOT;
const rename = fs.promises.rename;
fs.promises.rename = async function (from, to) {
  if (root && from === path.join(root, ".browser-build-lease")) {
    fs.writeFileSync(path.join(root, "retiring"), "");
    while (!fs.existsSync(path.join(root, "retire"))) await delay(5);
  }
  return rename(from, to);
};
syncBuiltinESMExports();
