const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { test } = require("node:test");

const launcher = fs.readFileSync(
  path.resolve(__dirname, "../../../../scripts/test-publisher-registration.sh"),
  "utf8",
);
const preflight = launcher.match(
  /if \[\[ "\$MODE" != prepare \]\]; then\n[\s\S]*?\nfi/,
);
assert.ok(
  preflight,
  "the test must execute the actual launcher runtime preflight",
);

for (const pin of ["0.17.0", "0.21.0"]) {
  test(`runtime preflight follows checkout pin ${pin}`, (t) => {
    const root = fs.mkdtempSync(
      path.join(os.tmpdir(), "publisher-registration-pin-"),
    );
    t.after(() => fs.rmSync(root, { recursive: true, force: true }));
    fs.writeFileSync(
      path.join(root, ".tool-versions"),
      `nodejs 24.12.0\nviceroy   ${pin}\n`,
    );
    const check = (actual, mode = "framework") =>
      spawnSync(
        "bash",
        [
          "-c",
          `set -eu\nviceroy() { printf '%s\\n' "$ACTUAL"; }\n${preflight[0]}`,
        ],
        {
          encoding: "utf8",
          env: { ...process.env, ROOT: root, MODE: mode, ACTUAL: actual },
        },
      );
    assert.equal(check(`viceroy ${pin}`).status, 0);
    const mismatch = check("viceroy 0.0.0");
    assert.equal(mismatch.status, 1);
    assert.match(
      mismatch.stderr,
      new RegExp(`Pinned Viceroy ${pin.replaceAll(".", "\\.")} required`),
    );
    assert.equal(
      check("viceroy 0.0.0", "prepare").status,
      0,
      "preparation needs no runtime",
    );
  });
}
