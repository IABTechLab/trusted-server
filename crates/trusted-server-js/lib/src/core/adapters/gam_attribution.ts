type GoogletagTarget = Window & { googletag?: unknown };

function object(value: unknown): Record<PropertyKey, unknown> | undefined {
  return (typeof value === 'object' && value !== null) || typeof value === 'function'
    ? (value as Record<PropertyKey, unknown>)
    : undefined;
}

/** Enqueue the document-local GAM attribution command at the parser-time boundary. */
export function enqueueFirstDisplayGamAttribution(target: GoogletagTarget): boolean {
  try {
    let binding = object(target.googletag);
    if (!binding) {
      binding = { cmd: [] };
      if (
        !Reflect.defineProperty(target, 'googletag', {
          configurable: true,
          enumerable: true,
          value: binding,
          writable: true,
        }) ||
        target.googletag !== binding
      ) {
        return false;
      }
    }
    const queue = object(Reflect.get(binding, 'cmd'));
    const push = queue && Reflect.get(queue, 'push');
    if (!queue || typeof push !== 'function') return false;
    Reflect.apply(push, queue, [
      () => {
        try {
          const root = object(target.googletag) ?? binding;
          const setConfig = Reflect.get(root, 'setConfig');
          if (typeof setConfig === 'function') {
            Reflect.apply(setConfig, root, [{ targeting: { ts: 'true' } }]);
          }
        } catch {
          // Missing or hostile GPT targeting cannot block later queue work.
        }
      },
    ]);
    return true;
  } catch {
    return false;
  }
}
