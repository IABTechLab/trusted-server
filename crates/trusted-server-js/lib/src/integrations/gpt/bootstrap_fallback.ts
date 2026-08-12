import { canPublishTerminalFields, prepareQueue, publishQueue } from '../../core/queue';
import { EMBEDDED_RELEASE_ID } from '../../core/release';
import { captureTrustedCriticalSrc, createFallbackFields } from '../../kernel/fallback';

function installGeneratedBootstrapFallback(
  target: object & { que?: unknown; boot?: unknown },
  trustedCriticalSrc: string
): boolean {
  const bootDescriptor = Object.getOwnPropertyDescriptor(target, 'boot');
  const boot = bootDescriptor && 'value' in bootDescriptor ? bootDescriptor.value : {};
  const fields = createFallbackFields({
    releaseId: EMBEDDED_RELEASE_ID,
    reason: 'bundle_partial',
    boot,
    trustedCriticalSrc,
  });
  if (!fields || !canPublishTerminalFields(target, fields)) return false;
  const ingress = prepareQueue(target);
  const published = publishQueue(target, ingress, fields);
  published.drain();
  return true;
}

function captureGeneratedCriticalSrc(runtimeDocument: Document): string | undefined {
  const scripts = runtimeDocument.querySelectorAll('script#trustedserver-js');
  const script = scripts.length === 1 ? scripts[0] : undefined;
  const Script = runtimeDocument.defaultView?.HTMLScriptElement;
  return Script && script instanceof Script
    ? captureTrustedCriticalSrc(runtimeDocument, script)
    : undefined;
}

const browser = (globalThis as unknown as { window?: { document: Document; tsjs?: unknown } })
  .window;
if (browser) {
  try {
    const trustedCriticalSrc = captureGeneratedCriticalSrc(browser.document);
    if (trustedCriticalSrc) {
      const namespaceDescriptor = Object.getOwnPropertyDescriptor(browser, 'tsjs');
      const existing =
        namespaceDescriptor && 'value' in namespaceDescriptor
          ? namespaceDescriptor.value
          : undefined;
      if (typeof existing === 'object' && existing !== null && !Array.isArray(existing)) {
        installGeneratedBootstrapFallback(
          existing as object & { que?: unknown; boot?: unknown },
          trustedCriticalSrc
        );
      } else {
        const target = {};
        if (installGeneratedBootstrapFallback(target, trustedCriticalSrc)) {
          Object.defineProperty(browser, 'tsjs', {
            configurable: true,
            enumerable: true,
            value: target,
            writable: true,
          });
        }
      }
    }
  } catch {
    // A namespace that cannot be defined cannot expose any fallback API safely.
  }
}
