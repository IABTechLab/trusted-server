import type { Size } from '../../core/types';

/** Formats CSS sizes consistently across diagnostics presentation surfaces. */
export function formatSizes(sizes: ReadonlyArray<Size>): string {
  return sizes.map((size) => `${size[0]}×${size[1]}`).join(', ');
}

/** Schedules presentation work in the target window's next animation frame. */
export function scheduleFrame(
  window: Pick<Window, 'requestAnimationFrame' | 'cancelAnimationFrame'>,
  callback: () => void
): () => void {
  if (typeof window.requestAnimationFrame === 'function') {
    let active = true;
    const frame = window.requestAnimationFrame(() => {
      if (active) callback();
    });
    return () => {
      active = false;
      if (typeof window.cancelAnimationFrame === 'function') {
        window.cancelAnimationFrame(frame);
      }
    };
  }

  let active = true;
  queueMicrotask(() => {
    if (active) callback();
  });
  return () => {
    active = false;
  };
}
