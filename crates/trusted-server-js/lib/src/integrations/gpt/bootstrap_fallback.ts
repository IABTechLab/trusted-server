import { canPublishTerminalFields, prepareQueue, publishQueue } from '../../core/queue';
import { EMBEDDED_RELEASE_ID } from '../../core/release';
import { createFallbackFields } from '../../kernel/fallback';

function installGeneratedBootstrapFallback(
  target: object & { que?: unknown; boot?: unknown }
): void {
  const bootDescriptor = Object.getOwnPropertyDescriptor(target, 'boot');
  const boot = bootDescriptor && 'value' in bootDescriptor ? bootDescriptor.value : {};
  const fields = createFallbackFields({
    releaseId: EMBEDDED_RELEASE_ID,
    reason: 'bundle_partial',
    boot,
  });
  if (!canPublishTerminalFields(target, fields)) return;
  const ingress = prepareQueue(target);
  const published = publishQueue(target, ingress, fields);
  published.drain();
}

const browser = (globalThis as unknown as { window?: { tsjs?: unknown } }).window;
if (browser) {
  try {
    const namespaceDescriptor = Object.getOwnPropertyDescriptor(browser, 'tsjs');
    const existing =
      namespaceDescriptor && 'value' in namespaceDescriptor ? namespaceDescriptor.value : undefined;
    let target: object & { que?: unknown; boot?: unknown };
    if (typeof existing === 'object' && existing !== null && !Array.isArray(existing)) {
      target = existing as object & { que?: unknown; boot?: unknown };
    } else {
      target = {};
      Object.defineProperty(browser, 'tsjs', {
        configurable: true,
        enumerable: true,
        value: target,
        writable: true,
      });
    }
    if (!Object.getOwnPropertyDescriptor(target, 'boot')) {
      Object.defineProperty(target, 'boot', {
        configurable: true,
        enumerable: true,
        value: {},
        writable: true,
      });
    }
    installGeneratedBootstrapFallback(target);
  } catch {
    // A namespace that cannot be defined cannot expose any fallback API safely.
  }
}
