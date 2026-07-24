import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { setTimeout as setTimeoutPromise } from "node:timers/promises";
import { createServer } from "node:net";
import { waitForReady } from "./wait-for-ready.js";

const ORIGIN_PORT = process.env.INTEGRATION_ORIGIN_PORT || "8888";

/** Framework-specific container configuration. */
const FRAMEWORK_CONFIG: Record<string, { image: string; port: number }> = {
  "ad-trace": { image: "test-ad-trace:latest", port: 80 },
  nextjs: { image: "test-nextjs:latest", port: 3000 },
  wordpress: { image: "test-wordpress:latest", port: 80 },
};

/** Find an available TCP port by briefly binding to port 0. */
export async function findAvailablePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address();
      if (addr && typeof addr === "object") {
        const port = addr.port;
        server.close(() => resolve(port));
      } else {
        server.close(() => reject(new Error("Failed to get port")));
      }
    });
    server.on("error", reject);
  });
}

/**
 * Start a Docker container for the given framework.
 *
 * Uses `docker run -d` (detached) with `--rm` for auto-cleanup on stop.
 * The container is mapped to the fixed origin port so the WASM binary
 * (which has this port baked in) can proxy to it.
 *
 * @returns The container ID.
 */
export async function startContainer(framework: string): Promise<string> {
  const config = FRAMEWORK_CONFIG[framework];
  if (!config) {
    throw new Error(
      `Unknown framework: ${framework}. Expected one of: ${Object.keys(FRAMEWORK_CONFIG).join(", ")}`,
    );
  }

  const containerId = execFileSync(
    "docker",
    [
      "run",
      "-d",
      "--rm",
      "-p",
      `${ORIGIN_PORT}:${config.port}`,
      "-e",
      `ORIGIN_HOST=127.0.0.1:${ORIGIN_PORT}`,
      config.image,
    ],
    { encoding: "utf-8" },
  ).trim();

  try {
    await waitForReady(`http://127.0.0.1:${ORIGIN_PORT}`, "/");
  } catch (err) {
    // Container started but never became ready — stop it before propagating.
    stopContainer(containerId);
    throw err;
  }

  return containerId;
}

/** Stop a Docker container by ID. */
export function stopContainer(containerId: string): void {
  try {
    execFileSync("docker", ["stop", containerId], { timeout: 10_000 });
  } catch {
    // Container may have already stopped (--rm flag)
  }
}

/**
 * Spawn a Viceroy process with the given WASM binary on a random port.
 *
 * Mirrors the Rust `FastlyEnvironment::spawn` in `tests/environments/fastly.rs`.
 *
 * @returns The child process and the base URL.
 */
export async function startViceroy(
  wasmPath: string,
  configPath: string,
): Promise<{ process: ChildProcess; baseUrl: string }> {
  const port = await findAvailablePort();

  const child = spawn(
    "viceroy",
    [wasmPath, "-C", configPath, "--addr", `127.0.0.1:${port}`],
    // stdin: ignore, stdout: pipe (drained), stderr: pipe (logged).
    // IMPORTANT: stdout MUST be drained to prevent pipe buffer deadlock.
    // If the OS pipe buffer (~64KB) fills, Viceroy blocks on write.
    { stdio: ["ignore", "pipe", "pipe"] },
  );

  // Drain stdout to prevent pipe buffer deadlock
  child.stdout?.resume();

  // Surface Viceroy stderr for debugging
  child.stderr?.on("data", (data: Buffer) => {
    const line = data.toString().trim();
    if (line) console.error(`[viceroy] ${line}`);
  });

  const baseUrl = `http://127.0.0.1:${port}`;

  try {
    await waitForReady(baseUrl, "/health");
  } catch (err) {
    // Viceroy spawned but never became ready — kill it before propagating.
    if (child.pid) await stopViceroy(child.pid);
    throw err;
  }

  return { process: child, baseUrl };
}

/**
 * Kill a Viceroy process and wait for it to fully exit.
 *
 * Sends SIGTERM then polls with a zero-signal probe until the process is gone,
 * escalating to SIGKILL if it has not exited within 5 seconds. Polling rather
 * than a blind sleep avoids both premature port-reuse races (too short) and
 * unnecessary CI delays (too long).
 *
 * `process.kill(pid, 0)` does not send a signal — it only checks whether the
 * process exists. It throws `ESRCH` once the OS has reaped the process.
 */
export async function stopViceroy(pid: number): Promise<void> {
  try {
    process.kill(pid, "SIGTERM");
  } catch {
    // Process already exited before we could signal it
    return;
  }

  // Poll until the process exits or the 5-second deadline is reached.
  const DEADLINE_MS = 5_000;
  const POLL_INTERVAL_MS = 50;
  const deadline = Date.now() + DEADLINE_MS;

  while (Date.now() < deadline) {
    await setTimeoutPromise(POLL_INTERVAL_MS);
    try {
      process.kill(pid, 0);
      // Process is still alive — keep polling
    } catch (err: unknown) {
      if ((err as { code?: string }).code === "ESRCH") {
        // Process has exited and the port is released
        return;
      }
      // EPERM or other unexpected error — do not treat as clean exit
      throw err;
    }
  }

  // SIGTERM did not finish within the deadline — escalate to SIGKILL
  try {
    process.kill(pid, "SIGKILL");
  } catch {
    // Process exited between the last poll and the SIGKILL attempt
  }
}
