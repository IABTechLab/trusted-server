import { readFileSync, unlinkSync } from "node:fs";
import { resolve } from "node:path";
import { stopContainer, stopViceroy } from "./helpers/infra.js";

const STATE_FILE = resolve(__dirname, ".browser-test-state.json");

async function globalTeardown(): Promise<void> {
  let state: { containerId?: string; viceroyPid?: number };
  try {
    state = JSON.parse(readFileSync(STATE_FILE, "utf-8"));
  } catch {
    console.warn("[global-teardown] No state file found, nothing to clean up");
    return;
  }

  if (state.viceroyPid) {
    console.log(`[global-teardown] Stopping Viceroy (pid: ${state.viceroyPid})`);
    stopViceroy(state.viceroyPid);
  }

  if (state.containerId) {
    console.log(`[global-teardown] Stopping container ${state.containerId.slice(0, 12)}...`);
    stopContainer(state.containerId);
  }

  try {
    unlinkSync(STATE_FILE);
  } catch {
    // Already removed
  }
}

export default globalTeardown;
