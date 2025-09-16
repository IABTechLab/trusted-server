export function delay(ms = 0): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

export function queueTask(callback: () => void): void {
  if (typeof queueMicrotask === 'function') {
    queueMicrotask(callback);
  } else {
    setTimeout(callback, 0);
  }
}
