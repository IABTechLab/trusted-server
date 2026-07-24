import { readFileSync } from "node:fs";
import { resolve } from "node:path";

export interface TestState {
  baseUrl: string;
  containerId: string;
  viceroyPid: number;
  framework: string;
}

const KNOWN_FRAMEWORKS = ["ad-trace", "nextjs", "wordpress"] as const;

const STATE_FILE = resolve(__dirname, "../.browser-test-state.json");
let cachedState: TestState | undefined;

/** Read the state written by global-setup.ts. */
export function readState(): TestState {
  const state: TestState = JSON.parse(readFileSync(STATE_FILE, "utf-8"));
  if (!KNOWN_FRAMEWORKS.includes(state.framework as (typeof KNOWN_FRAMEWORKS)[number])) {
    throw new Error(
      `Unknown framework "${state.framework}" in state file. Expected one of: ${KNOWN_FRAMEWORKS.join(", ")}`,
    );
  }
  return state;
}

/** Read the state once and reuse it for the rest of the test process. */
function getCachedState(): TestState {
  return (cachedState ??= readState());
}

/** Resolve an absolute runtime URL from the current browser test state. */
export function runtimeUrl(path: string): string {
  return new URL(path, getCachedState().baseUrl).toString();
}
