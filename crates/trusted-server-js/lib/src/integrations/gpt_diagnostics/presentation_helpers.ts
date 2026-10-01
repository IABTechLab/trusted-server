import type { Size } from '../../core/types';

/** Formats CSS sizes consistently across diagnostics presentation surfaces. */
export function formatSizes(sizes: ReadonlyArray<Size>): string {
  return sizes.map((size) => `${size[0]}×${size[1]}`).join(', ');
}

/** Schedules presentation work in the target window's next animation frame. */
export function scheduleFrame(
  window: Pick<Window, 'requestAnimationFrame'>,
  callback: () => void
): void {
  if (typeof window.requestAnimationFrame === 'function') {
    window.requestAnimationFrame(() => callback());
  } else {
    queueMicrotask(callback);
  }
}
