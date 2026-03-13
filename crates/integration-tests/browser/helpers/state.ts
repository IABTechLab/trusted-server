import { readFileSync } from "node:fs";
import { resolve } from "node:path";

export interface TestState {
  baseUrl: string;
  containerId: string;
  viceroyPid: number;
  framework: string;
}

const STATE_FILE = resolve(__dirname, "../.browser-test-state.json");

/** Read the state written by global-setup.ts. */
export function readState(): TestState {
  return JSON.parse(readFileSync(STATE_FILE, "utf-8"));
}

/** Resolve an absolute runtime URL from the current browser test state. */
export function runtimeUrl(path: string): string {
  return new URL(path, readState().baseUrl).toString();
}
