import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import {
  chmod,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

const entrypoint = await readFile(
  new URL("./entrypoint.sh", import.meta.url),
  "utf8",
);

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), "ts-native-bootstrap-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const runtime = join(root, "runtime");
  await mkdir(runtime, { mode: 0o700 });
  const app = join(root, "app.mjs");
  await writeFile(
    app,
    `#!${process.execPath}
import { readFileSync, statSync } from 'node:fs'
const file = process.env.EDGEZERO__STORE_BINDINGS_FILE
console.log(JSON.stringify({
  pid: process.pid,
  args: process.argv.slice(2),
  contents: readFileSync(file, 'utf8'),
  mode: statSync(file).mode & 0o777,
  configDir: process.env.EDGEZERO__ADAPTER__CONFIG_DIR ?? null,
  dataDir: process.env.EDGEZERO__ADAPTER__DATA_DIR ?? null,
  bindingsEnv: process.env.TS_NATIVE_BINDINGS_TOML ?? null,
  geoEnv: process.env.TS_NATIVE_GEO_PROFILE_TOML ?? null,
  geoFile: process.env.TS_NATIVE_GEO_PROFILE_FILE ?? null,
  geoContents: process.env.TS_NATIVE_GEO_PROFILE_FILE ? readFileSync(process.env.TS_NATIVE_GEO_PROFILE_FILE, 'utf8') : null,
  geoMode: process.env.TS_NATIVE_GEO_PROFILE_FILE ? statSync(process.env.TS_NATIVE_GEO_PROFILE_FILE).mode & 0o777 : null,
}))
if (process.env.WAIT_FOR_SIGNAL === 'true') {
  process.on('SIGTERM', () => process.exit(23))
  console.log('ready')
  setInterval(() => {}, 1000)
}
`,
    { mode: 0o755 },
  );
  // Only relocate fixed production paths. Exercise the unchanged shell logic.
  assert.equal(entrypoint.split("runtime_dir='/run/trusted-server'").length, 2);
  assert.equal(entrypoint.split('exec /usr/local/bin/app "$@"').length, 2);
  const script = join(root, "entrypoint.sh");
  await writeFile(
    script,
    entrypoint
      .replace("runtime_dir='/run/trusted-server'", `runtime_dir='${runtime}'`)
      .replace('exec /usr/local/bin/app "$@"', `exec '${app}' "$@"`),
  );
  const bindings = join(runtime, "native-bindings.toml");
  function start(input, extra = {}, args = []) {
    const env = {
      PATH: "/usr/bin:/bin",
      EDGEZERO__STORE_BINDINGS_FILE: bindings,
      EDGEZERO__ADAPTER__CONFIG_DIR: "/does-not-exist/config",
      EDGEZERO__ADAPTER__DATA_DIR: "/does-not-exist/data",
      ...extra,
    };
    if (input !== undefined) env.TS_NATIVE_BINDINGS_TOML = input;
    const child = spawn("/bin/sh", [script, ...args], {
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8").on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.setEncoding("utf8").on("data", (chunk) => {
      stderr += chunk;
    });
    const closed = once(child, "close").then(([code, signal]) => ({
      code,
      signal,
      stdout,
      stderr,
    }));
    t.after(() => {
      if (child.exitCode === null && child.signalCode === null)
        child.kill("SIGKILL");
    });
    return { child, closed };
  }
  return { root, runtime, bindings, start };
}

test("writes TOML literally with mode 0600, removes inherited local roots and execs the app", async (t) => {
  const { runtime, start } = await fixture(t);
  const payload =
    'version = 1\n# $HOME ${HOME} `false` $(false) %s \\\n[config.example]\nvalue = "literal"\n';
  const { child, closed } = start(payload, {}, ["one argument", "$HOME"]);
  const result = await closed;
  assert.equal(result.code, 0, result.stderr);
  assert.equal(result.stderr, "");
  const observed = JSON.parse(result.stdout);
  assert.equal(observed.contents, payload);
  assert.equal(observed.mode, 0o600);
  assert.equal(observed.pid, child.pid);
  assert.deepEqual(observed.args, ["one argument", "$HOME"]);
  assert.equal(observed.configDir, null);
  assert.equal(observed.dataDir, null);
  assert.equal(observed.bindingsEnv, null);
  assert.deepEqual(await readdir(runtime), ["native-bindings.toml"]);
});

test("Geo is Off without a profile, ignoring stale pointers and volume files", async (t) => {
  const { runtime, start } = await fixture(t);
  await writeFile(join(runtime, "native-geo.toml"), "stale");
  const result = await start("version = 1", {
    TS_NATIVE_GEO_PROFILE_FILE: "/missing",
  }).closed;
  assert.equal(result.code, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).geoFile, null);
  assert.equal(JSON.parse(result.stdout).geoContents, null);
});

test("publishes the literal operational Geo profile privately and exports only its pointer", async (t) => {
  const { root, runtime, start } = await fixture(t);
  const target = join(root, "untouched");
  await writeFile(target, "original");
  await symlink(target, join(runtime, "native-geo.toml"));
  const marker = join(root, "executed");
  const profile = `version = 1\n[geo]\nprovider = "cloudfront"\nalb_peer_cidrs = ["192.0.2.0/24"]\norigin_credential_header_name = "x-fictional-origin-check"\n# $(touch '${marker}')\n`;
  const result = await start("version = 1", {
    TS_NATIVE_GEO_PROFILE_TOML: profile,
  }).closed;
  assert.equal(result.code, 0, result.stderr);
  const observed = JSON.parse(result.stdout);
  assert.equal(observed.geoFile, join(runtime, "native-geo.toml"));
  assert.equal(observed.geoContents, profile);
  assert.equal(observed.geoMode, 0o600);
  assert.equal(observed.geoEnv, null);
  assert.equal(await readFile(target, "utf8"), "original");
  await assert.rejects(lstat(marker), { code: "ENOENT" });
  assert.deepEqual(await readdir(runtime), [
    "native-bindings.toml",
    "native-geo.toml",
  ]);
});

test("an explicitly empty or unpublishable Geo profile never launches the app", async (t) => {
  const { runtime, start } = await fixture(t);
  let result = await start("version = 1", { TS_NATIVE_GEO_PROFILE_TOML: "" })
    .closed;
  assert.notEqual(result.code, 0);
  assert.equal(result.stdout, "");
  assert.match(result.stderr, /TS_NATIVE_GEO_PROFILE_TOML must not be empty/);
  await mkdir(join(runtime, "native-geo.toml"));
  result = await start("version = 1", {
    TS_NATIVE_GEO_PROFILE_TOML: "fictional profile sentinel",
  }).closed;
  assert.notEqual(result.code, 0);
  assert.equal(result.stdout, "");
  assert.doesNotMatch(result.stderr, /fictional profile sentinel/);
  assert.deepEqual(await readdir(runtime), [
    "native-bindings.toml",
    "native-geo.toml",
  ]);
});

test("shell substitutions in TOML cannot execute", async (t) => {
  const { root, start } = await fixture(t);
  const marker = join(root, "executed");
  const payload = `# $(touch '${marker}')\n# \`touch '${marker}'\`\n`;
  const result = await start(payload).closed;
  assert.equal(result.code, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).contents, payload);
  await assert.rejects(lstat(marker), { code: "ENOENT" });
});

for (const input of [undefined, ""]) {
  test(`rejects ${input === undefined ? "missing" : "empty"} input without launching the app`, async (t) => {
    const { runtime, start } = await fixture(t);
    const result = await start(input).closed;
    assert.notEqual(result.code, 0);
    assert.equal(result.stdout, "");
    assert.match(result.stderr, /TS_NATIVE_BINDINGS_TOML is required/);
    assert.deepEqual(await readdir(runtime), []);
  });
}

test("rejects a missing runtime mount", async (t) => {
  const { runtime, start } = await fixture(t);
  await rm(runtime, { recursive: true });
  const result = await start("nonsecret sentinel").closed;
  assert.notEqual(result.code, 0);
  assert.equal(result.stdout, "");
  assert.match(result.stderr, /directory must be writable/);
  assert.doesNotMatch(result.stderr, /nonsecret sentinel/);
});

test(
  "rejects an unwritable runtime mount",
  { skip: process.getuid?.() === 0 && "root bypasses directory permissions" },
  async (t) => {
    const { runtime, start } = await fixture(t);
    await chmod(runtime, 0o500);
    t.after(() => chmod(runtime, 0o700).catch(() => {}));
    const result = await start("nonsecret sentinel").closed;
    assert.notEqual(result.code, 0);
    assert.equal(result.stdout, "");
    assert.match(result.stderr, /directory must be writable/);
    assert.doesNotMatch(result.stderr, /nonsecret sentinel/);
  },
);

test("replaces an existing symlink without touching its target and resets file permissions", async (t) => {
  const { root, runtime, bindings, start } = await fixture(t);
  const target = join(root, "untouched");
  await writeFile(target, "original", { mode: 0o644 });
  await symlink(target, bindings);
  const first = await start("first").closed;
  assert.equal(first.code, 0, first.stderr);
  assert.equal(await readFile(target, "utf8"), "original");
  assert.equal((await lstat(bindings)).isSymbolicLink(), false);
  await chmod(bindings, 0o666);
  const second = await start("second").closed;
  assert.equal(second.code, 0, second.stderr);
  assert.equal(JSON.parse(second.stdout).contents, "second");
  assert.equal(JSON.parse(second.stdout).mode, 0o600);
  assert.deepEqual(await readdir(runtime), ["native-bindings.toml"]);
});

test("failed publication does not launch the app or leave a temporary file", async (t) => {
  const { runtime, bindings, start } = await fixture(t);
  await mkdir(bindings);
  const result = await start("nonsecret sentinel").closed;
  assert.notEqual(result.code, 0);
  assert.equal(result.stdout, "");
  assert.doesNotMatch(result.stderr, /nonsecret sentinel/);
  assert.deepEqual(await readdir(runtime), ["native-bindings.toml"]);
});

test(
  "SIGTERM reaches the execed app directly",
  { timeout: 5000 },
  async (t) => {
    const { start } = await fixture(t);
    const { child, closed } = start("version = 1", { WAIT_FOR_SIGNAL: "true" });
    let output = "";
    for await (const chunk of child.stdout) {
      output += chunk;
      if (output.includes("\nready\n")) break;
    }
    assert.match(output, /\nready\n/);
    child.kill("SIGTERM");
    const result = await closed;
    assert.equal(result.code, 23, result.stderr);
    assert.equal(result.signal, null);
  },
);

test("image contract requires separate digest-pinned binary/runtime inputs, owned volume and non-root wrapper", async () => {
  const dockerfile = await readFile(
    new URL("./Dockerfile", import.meta.url),
    "utf8",
  );
  assert.match(dockerfile, /^ARG TS_NATIVE_PROVIDER_IMAGE$/m);
  assert.match(dockerfile, /^ARG TS_NATIVE_RUNTIME_IMAGE$/m);
  assert.match(
    dockerfile,
    /^FROM \$\{TS_NATIVE_PROVIDER_IMAGE\} AS binary-source$/m,
  );
  assert.match(dockerfile, /^FROM \$\{TS_NATIVE_RUNTIME_IMAGE\} AS runtime$/m);
  assert.match(dockerfile, /grep -Eq '@sha256:\[a-f0-9\]\{64\}\$'/);
  assert.match(dockerfile, /test "\$ID:\$VERSION_ID" = debian:13/);
  assert.match(
    dockerfile,
    /^COPY --from=binary-source --chmod=0555 \/usr\/local\/bin\/app \/usr\/local\/bin\/app$/m,
  );
  assert.match(
    dockerfile,
    /^COPY --from=binary-source \/usr\/local\/share\/ts-build-inputs.sha256 \/usr\/local\/share\/ts-build-inputs.sha256$/m,
  );
  assert.match(
    dockerfile,
    /install -d -m 0700 -o 10001 -g 10001 \/run\/trusted-server/,
  );
  assert.match(dockerfile, /^ENV EDGEZERO__ADAPTER__HOST=0.0.0.0 \\/m);
  assert.match(dockerfile, /^STOPSIGNAL SIGTERM$/m);
  assert.match(dockerfile, /^USER 10001:10001$/m);
  assert.match(dockerfile, /^VOLUME \["\/run\/trusted-server"\]$/m);
  assert.match(
    dockerfile,
    /^ENTRYPOINT \["\/usr\/local\/bin\/ts-native-entrypoint"\]$/m,
  );
  assert.doesNotMatch(entrypoint, /\b(?:chown|eval|curl|aws)\b/);
});

test("Dockerfile-specific context permits only the bootstrap script without widening the root context", async () => {
  const ignore = await readFile(
    new URL("./Dockerfile.dockerignore", import.meta.url),
    "utf8",
  );
  const rules = ignore
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("#"));
  assert.deepEqual(rules, [
    "**",
    "!deploy/",
    "!deploy/runtime/",
    "!deploy/runtime/native-aws/",
    "!deploy/runtime/native-aws/entrypoint.sh",
  ]);
  const rootIgnore = await readFile(
    new URL("../../../.dockerignore", import.meta.url),
    "utf8",
  );
  assert(rootIgnore.split("\n").includes("**"));
  assert(!rootIgnore.split("\n").some((line) => line.startsWith("!deploy")));
});
