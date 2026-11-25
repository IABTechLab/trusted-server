/**
 * Poll a URL until it returns HTTP 200 without a redirect.
 *
 * Uses the same timing as the Rust readiness polling: 30 attempts with
 * 500ms intervals.
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
      const resp = await fetch(url, { redirect: "manual" });
      if (resp.status === 200) return;
    } catch {
      // Connection refused — server not ready yet
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }

  throw new Error(
    `Service at ${url} not ready after ${maxAttempts} attempts (${(maxAttempts * intervalMs) / 1000}s)`,
  );
}
