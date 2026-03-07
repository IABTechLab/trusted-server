import { execSync, spawn, type ChildProcess } from "node:child_process";
import { createServer } from "node:net";
import { waitForReady } from "./wait-for-ready.js";

const ORIGIN_PORT = process.env.INTEGRATION_ORIGIN_PORT || "8888";

/** Framework-specific container configuration. */
const FRAMEWORK_CONFIG: Record<string, { image: string; port: number }> = {
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

  const containerId = execSync(
    [
      "docker run -d --rm",
      `-p ${ORIGIN_PORT}:${config.port}`,
      `-e ORIGIN_HOST=127.0.0.1:${ORIGIN_PORT}`,
      config.image,
    ].join(" "),
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
    execSync(`docker stop ${containerId}`, { timeout: 10_000 });
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
    await waitForReady(baseUrl, "/__trusted-server/health");
  } catch (err) {
    // Viceroy spawned but never became ready — kill it before propagating.
    if (child.pid) stopViceroy(child.pid);
    throw err;
  }

  return { process: child, baseUrl };
}

/** Kill a Viceroy process. */
export function stopViceroy(pid: number): void {
  try {
    process.kill(pid, "SIGTERM");
  } catch {
    // Already exited
  }
}
