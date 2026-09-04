import type { GptDiagnosticsAuctionType, Size } from '../../core/types';

/** Formats CSS sizes consistently across diagnostics presentation surfaces. */
export function formatSizes(sizes: ReadonlyArray<Size>): string {
  return sizes.map((size) => `${size[0]}×${size[1]}`).join(', ');
}

/** Hide GPT's ubiquitous 1×1 placeholder from presentation while retaining it in exports. */
export function displayableGptFillSize(size: Size | undefined): Size | undefined {
  return size?.[0] === 1 && size[1] === 1 ? undefined : size;
}

/** Human-readable auction classification shared by the badge and side panel. */
export function auctionTypeLabel(type: GptDiagnosticsAuctionType): string {
  switch (type) {
    case 'ssat':
      return 'SSAT';
    case 'trusted_server':
      return 'TS auction';
    case 'client_side':
      return 'Client-side auction';
    case 'competing':
      return 'Competing auctions';
  }
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
