/** Samples native IDs while this document is active, including BFCache restores. */
export function installNativeIdCapture(capture: () => void): void {
  let timer: ReturnType<typeof setInterval> | undefined;
  const start = (): void => {
    if (timer !== undefined) return;
    timer = setInterval(capture, 1000);
    capture();
  };
  const onVisibilityChange = (): void => {
    if (timer !== undefined) capture();
  };
  const onPageHide = (event: PageTransitionEvent): void => {
    clearInterval(timer);
    timer = undefined;
    if (!event.persisted) {
      window.removeEventListener('pagehide', onPageHide);
      window.removeEventListener('pageshow', start);
      document.removeEventListener('visibilitychange', onVisibilityChange);
    }
  };
  window.addEventListener('pagehide', onPageHide);
  window.addEventListener('pageshow', start);
  document.addEventListener('visibilitychange', onVisibilityChange);
  start();
}
