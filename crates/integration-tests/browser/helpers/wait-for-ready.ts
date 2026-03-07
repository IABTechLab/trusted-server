/**
 * Poll a URL until it returns a successful response.
 *
 * Mirrors the Rust `wait_for_ready` implementation in
 * `tests/environments/mod.rs` — 30 attempts with 500ms intervals.
 */
export async function waitForReady(
  baseUrl: string,
  path: string,
  {
    maxAttempts = 30,
    intervalMs = 500,
  }: { maxAttempts?: number; intervalMs?: number } = {},
): Promise<void> {
  const url = `${baseUrl}${path}`;

  for (let i = 0; i < maxAttempts; i++) {
    try {
      const resp = await fetch(url);
      if (resp.ok) return;
    } catch {
      // Connection refused — server not ready yet
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }

  throw new Error(
    `Service at ${url} not ready after ${maxAttempts} attempts (${(maxAttempts * intervalMs) / 1000}s)`,
  );
}
