import { writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { execFileSync } from "node:child_process";
import {
  startContainer,
  startViceroy,
  stopContainer,
  stopViceroy,
} from "./helpers/infra.js";

const STATE_FILE = resolve(__dirname, ".browser-test-state.json");

const WASM_PATH =
  process.env.WASM_BINARY_PATH ||
  resolve(
    __dirname,
    "../../../target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm",
  );

const VICEROY_TEMPLATE = resolve(
  __dirname,
  "../fixtures/configs/viceroy-template.toml",
);
const APP_CONFIG = resolve(
  __dirname,
  "../fixtures/configs/trusted-server.integration.toml",
);
const RENDER_SCRIPT = resolve(
  __dirname,
  "../../../scripts/render-fastly-local-config.py",
);

/** Persist current state so global-teardown can always clean up. */
function writeState(state: {
  baseUrl?: string;
  containerId?: string;
  renderedConfigPath?: string;
  viceroyPid?: number;
  framework: string;
}): void {
  writeFileSync(STATE_FILE, JSON.stringify(state, null, 2));
}

async function globalSetup(): Promise<void> {
  const framework = process.env.TEST_FRAMEWORK || "nextjs";
  let containerId: string | undefined;
  let renderedConfig: string | undefined;
  let viceroyPid: number | undefined;

  try {
    console.log(`[global-setup] Starting ${framework} container...`);
    containerId = await startContainer(framework);

    // Write partial state immediately so teardown can stop the container
    // even if Viceroy startup fails below.
    writeState({ containerId, framework });

    renderedConfig = resolve(
      tmpdir(),
      `trusted-server-browser-${Date.now()}.toml`,
    );
    execFileSync("python3", [
      RENDER_SCRIPT,
      "--app-config",
      APP_CONFIG,
      "--template",
      VICEROY_TEMPLATE,
      "--output",
      renderedConfig,
    ]);

    console.log(`[global-setup] Starting Viceroy (WASM: ${WASM_PATH})...`);
    const viceroy = await startViceroy(WASM_PATH, renderedConfig);
    viceroyPid = viceroy.process.pid;

    console.log(`[global-setup] Viceroy ready at ${viceroy.baseUrl}`);

    // Write complete state for tests and teardown
    writeState({
      baseUrl: viceroy.baseUrl,
      containerId,
      renderedConfigPath: renderedConfig,
      viceroyPid,
      framework,
    });
  } catch (err) {
    // Clean up any resources that were started before re-throwing
    console.error("[global-setup] Setup failed, cleaning up...");
    if (viceroyPid) await stopViceroy(viceroyPid);
    if (containerId) stopContainer(containerId);
    try {
      const { unlinkSync } = await import("node:fs");
      if (renderedConfig) unlinkSync(renderedConfig);
    } catch {
      // Rendered config may not exist
    }

    // Remove partial state file since we cleaned up manually
    try {
      const { unlinkSync } = await import("node:fs");
      unlinkSync(STATE_FILE);
    } catch {
      // State file may not exist
    }

    throw err;
  }
}

export default globalSetup;
