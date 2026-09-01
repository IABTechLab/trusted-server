import { EMBEDDED_RELEASE_ID } from '../../core/release';
import { isGptIntegrationConfigV1 } from '../../shared/integration_config_validators';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';

type GptLaterNavigationResult =
  | Readonly<{
      status: 'committed';
      navigationGeneration: object;
      current: true;
    }>
  | Readonly<{
      status: 'rejected';
      navigationGeneration: object;
      current: boolean;
    }>;

interface GptLaterCapabilityV1 {
  readonly activateLaterLifecycle: () => Readonly<{
    readonly navigate: (path: string) => PromiseLike<GptLaterNavigationResult>;
    readonly release: () => void;
  }>;
}

interface RuntimeDocumentCapability {
  readonly document: Document;
}

function restoreHistoryMethod(
  history: History,
  name: 'pushState' | 'replaceState',
  previous: PropertyDescriptor | undefined,
  installed: History['pushState']
): void {
  try {
    const current = Object.getOwnPropertyDescriptor(history, name);
    if (!current || !('value' in current) || current.value !== installed) return;
    if (previous) Object.defineProperty(history, name, previous);
    else Reflect.deleteProperty(history, name);
  } catch {
    // A publisher replacement remains authoritative; the disposed wrapper is inert.
  }
}

/** Build the release-bound post-first-display GPT registration. */
export function createGptLaterIntegrationRegistration(releaseId: string): IntegrationRegistration {
  return Object.freeze({
    abi: 1,
    id: 'gpt_later',
    phase: 'deferred',
    releaseId,
    prepare: ({ config, interfaces }: IntegrationPrepareContext) => {
      const gpt = interfaces['gpt.v1'] as GptLaterCapabilityV1 | undefined;
      const runtime = interfaces['runtime.v1'] as RuntimeDocumentCapability | undefined;
      for (const key of ['slots.v1', 'auction.v1', 'render.v1', 'trace.v1']) {
        const capability = interfaces[key];
        if (typeof capability !== 'object' || capability === null || !Object.isFrozen(capability)) {
          throw new TypeError(`GPT later requires ${key}`);
        }
      }
      if (
        !isGptIntegrationConfigV1(config) ||
        !runtime ||
        !Object.isFrozen(runtime) ||
        !runtime.document?.defaultView ||
        !gpt ||
        !Object.isFrozen(gpt) ||
        typeof gpt.activateLaterLifecycle !== 'function'
      ) {
        throw new TypeError('GPT later capability graph is invalid');
      }
      const pageBidsEnabled = config.pageBidsEnabled;
      return Object.freeze({
        activate: ({ onDispose }: IntegrationActivationContext) => {
          const candidateView = runtime.document.defaultView;
          if (!candidateView) throw new TypeError('GPT later document is unavailable');
          const view: Window = candidateView;
          const history = view.history;
          const previousPushState = Object.getOwnPropertyDescriptor(history, 'pushState');
          const previousReplaceState = Object.getOwnPropertyDescriptor(history, 'replaceState');
          const pushState = history.pushState;
          const replaceState = history.replaceState;
          let active = true;
          let timer: number | undefined;
          let lastCommittedPath = `${view.location.pathname}${view.location.search}`;
          let observedPath = lastCommittedPath;
          let pendingPath: string | undefined;
          let invocationOrdinal = 0;
          let latestInvocationOrdinal = 0;
          let owner: ReturnType<GptLaterCapabilityV1['activateLaterLifecycle']> | undefined;
          let wrappedPushState: History['pushState'] | undefined;
          let wrappedReplaceState: History['replaceState'] | undefined;
          const dispose = (): void => {
            if (!active) return;
            active = false;
            if (timer !== undefined) view.clearTimeout(timer);
            timer = undefined;
            pendingPath = undefined;
            view.removeEventListener('popstate', scheduleNavigation);
            if (wrappedReplaceState) {
              restoreHistoryMethod(
                history,
                'replaceState',
                previousReplaceState,
                wrappedReplaceState
              );
            }
            if (wrappedPushState) {
              restoreHistoryMethod(history, 'pushState', previousPushState, wrappedPushState);
            }
            const release = owner?.release;
            owner = undefined;
            release?.();
          };
          const flushNavigation = (): void => {
            timer = undefined;
            const path = pendingPath;
            pendingPath = undefined;
            if (!active || path === undefined || !owner) return;
            invocationOrdinal += 1;
            const invocation = invocationOrdinal;
            latestInvocationOrdinal = invocation;
            const rejectCurrentInvocation = (): void => {
              if (!active || invocation !== latestInvocationOrdinal) return;
              observedPath = lastCommittedPath;
            };
            try {
              void Promise.resolve(owner.navigate(path)).then((result) => {
                if (!active || invocation !== latestInvocationOrdinal) return;
                if (result.status === 'committed' && result.current) {
                  lastCommittedPath = path;
                  observedPath = path;
                  return;
                }
                if (result.status === 'rejected' && result.current) {
                  rejectCurrentInvocation();
                }
              }, rejectCurrentInvocation);
            } catch {
              rejectCurrentInvocation();
            }
          };
          function scheduleNavigation(): void {
            if (!active) return;
            let path: string;
            try {
              path = `${view.location.pathname}${view.location.search}`;
            } catch {
              return;
            }
            if (path === observedPath) return;
            observedPath = path;
            pendingPath = path;
            if (timer === undefined) timer = view.setTimeout(flushNavigation, 0);
          }
          const wrap = (original: History['pushState']): History['pushState'] =>
            function wrappedHistoryState(
              this: History,
              data: unknown,
              unused: string,
              url?: string | URL | null
            ): void {
              Reflect.apply(original, this, [data, unused, url]);
              scheduleNavigation();
            };
          onDispose(dispose);
          try {
            owner = gpt.activateLaterLifecycle();
            if (
              !owner ||
              !Object.isFrozen(owner) ||
              typeof owner.navigate !== 'function' ||
              typeof owner.release !== 'function'
            ) {
              throw new TypeError('GPT later lifecycle owner is invalid');
            }
            if (!pageBidsEnabled) return;
            wrappedPushState = wrap(pushState);
            wrappedReplaceState = wrap(replaceState);
            Object.defineProperty(history, 'pushState', {
              configurable: true,
              enumerable: previousPushState?.enumerable ?? false,
              value: wrappedPushState,
              writable: true,
            });
            Object.defineProperty(history, 'replaceState', {
              configurable: true,
              enumerable: previousReplaceState?.enumerable ?? false,
              value: wrappedReplaceState,
              writable: true,
            });
            view.addEventListener('popstate', scheduleNavigation);
          } catch (error) {
            dispose();
            throw error;
          }
        },
      });
    },
  });
}

if (typeof window !== 'undefined') {
  const register = (window.tsjs as unknown as { _registerIntegration?: unknown } | undefined)
    ?._registerIntegration;
  if (typeof register === 'function') {
    Reflect.apply(register, window.tsjs, [
      createGptLaterIntegrationRegistration(EMBEDDED_RELEASE_ID),
    ]);
  }
}
