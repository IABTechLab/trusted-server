import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPOSITORY_ROOT = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../..",
);
const FORBIDDEN_EXTENSIONS = new Set([".har", ".zip", ".webm"]);
const FORBIDDEN_FIELD =
  /"(?:accountId|aaxResponse|adm|authorization|capabilities?|creativeBody|descriptor|lifecycleTicket|nonce|postData|requestHeaders|responseBody|responseHeaders)"\s*:/u;
const DOCUMENT_BODY = /<script(?:\s|>)|<!doctype/iu;

function evidenceRoots(repositoryRoot) {
  return {
    quality: path.join(repositoryRoot, "target/aps-tsjs-quality-evidence"),
    cutover: path.join(repositoryRoot, "target/aps-tsjs-cutover-evidence"),
    realGam: path.join(
      repositoryRoot,
      "crates/trusted-server-integration-tests/browser",
    ),
  };
}

function requiredEnvironment(environment, name) {
  const value = environment[name];
  if (!value) throw new Error(`${name} must be nonempty`);
  return value;
}

function writeManifest(file, manifest) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(manifest, null, 2)}\n`, {
    mode: 0o600,
  });
}

function baseManifest(environment, conclusion) {
  return {
    schemaVersion: 1,
    evidenceId: requiredEnvironment(environment, "EVIDENCE_ID"),
    releaseId: requiredEnvironment(environment, "RELEASE_ID"),
    commitSha: requiredEnvironment(environment, "GITHUB_SHA"),
    runId: requiredEnvironment(environment, "GITHUB_RUN_ID"),
    conclusion,
  };
}

function walkFiles(root) {
  if (!fs.existsSync(root)) return [];
  const files = [];
  const walk = (directory) => {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const entryPath = path.join(directory, entry.name);
      if (entry.isDirectory()) walk(entryPath);
      else files.push(entryPath);
    }
  };
  walk(root);
  return files;
}

function assertAllowedExtension(file) {
  const extension = path.extname(file);
  if (FORBIDDEN_EXTENSIONS.has(extension)) {
    throw new Error(`native capture forbidden: ${file}`);
  }
}

function writeQualityManifest(root, environment) {
  writeManifest(
    path.join(root, "evidence-manifest.json"),
    baseManifest(environment, "success"),
  );
}

function writeIntegrationManifest(root, environment) {
  writeManifest(path.join(root, "evidence-manifest.json"), {
    ...baseManifest(environment, "success"),
    previousArtifactId: requiredEnvironment(
      environment,
      "PREVIOUS_ARTIFACT_ID",
    ),
  });
}

function writeRealGamManifest(root, environment) {
  writeManifest(path.join(root, "real-gam-evidence/evidence-manifest.json"), {
    ...baseManifest(
      environment,
      requiredEnvironment(environment, "TEST_OUTCOME"),
    ),
    previousArtifactId: requiredEnvironment(
      environment,
      "PREVIOUS_ARTIFACT_ID",
    ),
  });
}

function scrubIntegration(root, environment) {
  const secret = requiredEnvironment(environment, "INTEGRATION_AUTHORIZATION");
  for (const file of walkFiles(root)) {
    assertAllowedExtension(file);
    if (path.extname(file) === ".png") continue;
    const text = fs.readFileSync(file, "utf8");
    if (
      text.includes(secret) ||
      FORBIDDEN_FIELD.test(text) ||
      DOCUMENT_BODY.test(text)
    ) {
      throw new Error(
        `unsafe payload or capability found in evidence: ${file}`,
      );
    }
  }
}

function scrubRealGam(realGamRoot, environment) {
  const roots = ["real-gam-evidence", "playwright-report", "test-results"].map(
    (directory) => path.join(realGamRoot, directory),
  );
  const secrets = [
    requiredEnvironment(environment, "TS_REAL_GAM_PAGE_URL"),
    requiredEnvironment(environment, "TS_REAL_GAM_AUTH_HEADER"),
  ].map((secret) => Buffer.from(secret));
  const files = roots.flatMap(walkFiles);

  for (const file of files) {
    assertAllowedExtension(file);
    const body = fs.readFileSync(file);
    if (secrets.some((secret) => body.includes(secret))) {
      throw new Error(`protected value found in browser evidence: ${file}`);
    }
  }

  const traces = files.filter((file) =>
    file.endsWith("sanitized-trace-v1.json"),
  );
  for (const file of traces) {
    const text = fs.readFileSync(file, "utf8");
    if (FORBIDDEN_FIELD.test(text) || DOCUMENT_BODY.test(text)) {
      throw new Error(
        `unsafe field or body found in sanitized evidence: ${file}`,
      );
    }
  }
  if (
    requiredEnvironment(environment, "TEST_OUTCOME") === "success" &&
    traces.length !== 60
  ) {
    throw new Error(
      "successful three-browser run must emit 60 sanitized case traces",
    );
  }
}

function exactManifestKeys(value, expected) {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("real-GAM evidence manifest must be an object");
  }
  assert.deepEqual(
    Object.keys(value).sort(),
    [...expected].sort(),
    "real-GAM evidence manifest schema must be exact",
  );
}

export function validateRealGamArtifact(downloadRoot, expected) {
  const evidenceRoot = path.join(downloadRoot, "real-gam-evidence");
  const manifestPath = path.join(evidenceRoot, "evidence-manifest.json");
  const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
  exactManifestKeys(manifest, [
    "schemaVersion",
    "evidenceId",
    "releaseId",
    "commitSha",
    "runId",
    "conclusion",
    "previousArtifactId",
  ]);
  assert.equal(
    manifest.schemaVersion,
    1,
    "real-GAM evidence schema must be v1",
  );
  assert.equal(
    manifest.evidenceId,
    expected.evidenceId,
    "real-GAM evidence id must match",
  );
  assert.equal(
    manifest.releaseId,
    expected.releaseId,
    "real-GAM release id must match",
  );
  assert.equal(
    manifest.commitSha,
    expected.commitSha,
    "real-GAM commit must match",
  );
  assert.equal(
    String(manifest.runId),
    String(expected.runId),
    "real-GAM run id must match",
  );
  assert.equal(
    manifest.previousArtifactId,
    expected.previousArtifactId,
    "real-GAM rollback artifact must match",
  );
  assert.equal(
    manifest.conclusion,
    "success",
    "real-GAM evidence requires a successful conclusion",
  );

  const files = walkFiles(downloadRoot);
  for (const file of files) {
    assertAllowedExtension(file);
  }
  const traces = walkFiles(evidenceRoot).filter((file) =>
    file.endsWith("sanitized-trace-v1.json"),
  );
  for (const file of traces) {
    const text = fs.readFileSync(file, "utf8");
    if (FORBIDDEN_FIELD.test(text) || DOCUMENT_BODY.test(text)) {
      throw new Error(
        `unsafe field or body found in sanitized evidence: ${file}`,
      );
    }
  }
  assert.equal(
    traces.length,
    60,
    "successful real-GAM evidence must contain 60 traces",
  );
  return manifest;
}

function execute(
  command,
  repositoryRoot = REPOSITORY_ROOT,
  environment = process.env,
) {
  const roots = evidenceRoots(repositoryRoot);
  const commands = new Map([
    ["write-quality", () => writeQualityManifest(roots.quality, environment)],
    [
      "write-integration",
      () => writeIntegrationManifest(roots.cutover, environment),
    ],
    ["write-real-gam", () => writeRealGamManifest(roots.realGam, environment)],
    ["scrub-integration", () => scrubIntegration(roots.cutover, environment)],
    ["scrub-real-gam", () => scrubRealGam(roots.realGam, environment)],
  ]);
  const operation = commands.get(command);
  if (!operation) return false;
  operation();
  return true;
}

function selfTest() {
  const repositoryRoot = fs.mkdtempSync(
    path.join(os.tmpdir(), "aps-tsjs-evidence-"),
  );
  const environment = {
    EVIDENCE_ID: "aps-tsjs-self-test",
    RELEASE_ID: "release-self-test",
    PREVIOUS_ARTIFACT_ID: "previous-self-test",
    GITHUB_SHA: "0123456789abcdef0123456789abcdef01234567",
    GITHUB_RUN_ID: "12345",
    INTEGRATION_AUTHORIZATION: "integration-self-test-secret",
    TEST_OUTCOME: "success",
    TS_REAL_GAM_PAGE_URL: "https://real-gam.invalid/test",
    TS_REAL_GAM_AUTH_HEADER: "real-gam-self-test-secret",
  };

  try {
    execute("write-quality", repositoryRoot, environment);
    execute("write-integration", repositoryRoot, environment);
    execute("write-real-gam", repositoryRoot, environment);
    const roots = evidenceRoots(repositoryRoot);
    const quality = JSON.parse(
      fs.readFileSync(
        path.join(roots.quality, "evidence-manifest.json"),
        "utf8",
      ),
    );
    const integration = JSON.parse(
      fs.readFileSync(
        path.join(roots.cutover, "evidence-manifest.json"),
        "utf8",
      ),
    );
    assert.deepEqual(quality, {
      schemaVersion: 1,
      evidenceId: environment.EVIDENCE_ID,
      releaseId: environment.RELEASE_ID,
      commitSha: environment.GITHUB_SHA,
      runId: environment.GITHUB_RUN_ID,
      conclusion: "success",
    });
    assert.equal(
      integration.previousArtifactId,
      environment.PREVIOUS_ARTIFACT_ID,
    );

    fs.writeFileSync(path.join(roots.cutover, "safe.log"), "safe evidence\n");
    execute("scrub-integration", repositoryRoot, environment);
    fs.writeFileSync(
      path.join(roots.cutover, "unsafe.log"),
      '{"descriptor":"secret"}\n',
    );
    assert.throws(
      () => execute("scrub-integration", repositoryRoot, environment),
      /unsafe payload or capability/u,
    );
    fs.rmSync(path.join(roots.cutover, "unsafe.log"));
    fs.writeFileSync(
      path.join(roots.cutover, "capture.har"),
      "native capture\n",
    );
    assert.throws(
      () => execute("scrub-integration", repositoryRoot, environment),
      /native capture forbidden/u,
    );

    const traceRoot = path.join(roots.realGam, "real-gam-evidence");
    for (let index = 0; index < 60; index += 1) {
      fs.writeFileSync(
        path.join(traceRoot, `case-${index}-sanitized-trace-v1.json`),
        "{}\n",
      );
    }
    execute("scrub-real-gam", repositoryRoot, environment);
    const expectedRealGam = {
      evidenceId: environment.EVIDENCE_ID,
      releaseId: environment.RELEASE_ID,
      commitSha: environment.GITHUB_SHA,
      runId: environment.GITHUB_RUN_ID,
      previousArtifactId: environment.PREVIOUS_ARTIFACT_ID,
    };
    const reportRoot = path.join(roots.realGam, "playwright-report");
    fs.mkdirSync(reportRoot, { recursive: true });
    fs.writeFileSync(
      path.join(reportRoot, "index.html"),
      "<!doctype html><script>window.__playwrightReport = true;</script>\n",
    );
    validateRealGamArtifact(roots.realGam, expectedRealGam);
    const manifestPath = path.join(traceRoot, "evidence-manifest.json");
    const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
    fs.writeFileSync(
      manifestPath,
      `${JSON.stringify({ ...manifest, conclusion: "failure" })}\n`,
    );
    assert.throws(
      () => validateRealGamArtifact(roots.realGam, expectedRealGam),
      /successful conclusion/u,
    );
    fs.writeFileSync(manifestPath, `${JSON.stringify(manifest)}\n`);
    fs.writeFileSync(
      path.join(traceRoot, "secret.txt"),
      environment.TS_REAL_GAM_AUTH_HEADER,
    );
    assert.throws(
      () => execute("scrub-real-gam", repositoryRoot, environment),
      /protected value found/u,
    );
    fs.rmSync(path.join(traceRoot, "secret.txt"));
    fs.rmSync(path.join(traceRoot, "case-59-sanitized-trace-v1.json"));
    assert.throws(
      () => execute("scrub-real-gam", repositoryRoot, environment),
      /must emit 60 sanitized case traces/u,
    );
  } finally {
    fs.rmSync(repositoryRoot, { recursive: true, force: true });
  }
  process.stdout.write("APS/TSJS evidence self-test passed\n");
}

const command = process.argv[2];
if (command === "self-test") selfTest();
else if (command === "validate-real-gam") {
  const [
    downloadRoot,
    evidenceId,
    releaseId,
    commitSha,
    runId,
    previousArtifactId,
  ] = process.argv.slice(3);
  if (
    !downloadRoot ||
    !evidenceId ||
    !releaseId ||
    !/^[0-9a-f]{40}$/u.test(commitSha ?? "") ||
    !/^[1-9][0-9]*$/u.test(runId ?? "") ||
    !previousArtifactId ||
    process.argv.length !== 9
  ) {
    throw new Error(
      "usage: aps-tsjs-evidence.mjs validate-real-gam <download-root> <evidence-id> <release-id> <commit-sha> <run-id> <previous-artifact-id>",
    );
  }
  validateRealGamArtifact(downloadRoot, {
    evidenceId,
    releaseId,
    commitSha,
    runId,
    previousArtifactId,
  });
  process.stdout.write("APS real-GAM evidence is valid\n");
} else if (!execute(command)) {
  throw new Error(
    "usage: aps-tsjs-evidence.mjs {write-quality|write-integration|write-real-gam|scrub-integration|scrub-real-gam|validate-real-gam|self-test}",
  );
}
