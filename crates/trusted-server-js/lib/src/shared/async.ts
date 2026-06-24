// Async helpers shared by creative/core runtimes for yielding control.
// Simple Promise-based timeout helper.
export function delay(ms = 0): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

// Prefer microtasks when available so we preserve event ordering.
export function queueTask(callback: () => void): void {
  if (typeof queueMicrotask === 'function') {
    queueMicrotask(callback);
  } else {
    setTimeout(callback, 0);
  }
}
