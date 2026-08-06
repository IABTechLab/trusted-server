export type DisposeCallback = () => void;
export type DisposalErrorHandler = (error: unknown) => void;

const ignoreDisposalError: DisposalErrorHandler = () => undefined;

function isThenable(value: unknown): value is PromiseLike<unknown> {
  return (
    (typeof value === 'object' || typeof value === 'function') &&
    value !== null &&
    typeof (value as { then?: unknown }).then === 'function'
  );
}

/**
 * A synchronous, owned disposer stack for browser targets that do not provide the
 * TC39 DisposableStack proposal.
 */
export class DisposableStack {
  private readonly abortController = new AbortController();
  private readonly callbacks: DisposeCallback[] = [];
  private isDisposed = false;

  public constructor(private readonly onError: DisposalErrorHandler = ignoreDisposalError) {}

  public get disposed(): boolean {
    return this.isDisposed;
  }

  public get signal(): AbortSignal {
    return this.abortController.signal;
  }

  public onDispose(callback: DisposeCallback): void {
    if (typeof callback !== 'function') {
      throw new TypeError('A disposer must be a function');
    }

    if (this.isDisposed) {
      this.run(callback);
      return;
    }

    this.callbacks.push(callback);
  }

  public dispose(): void {
    if (this.isDisposed) return;
    this.isDisposed = true;
    this.abortController.abort();

    for (let index = this.callbacks.length - 1; index >= 0; index -= 1) {
      const callback = this.callbacks[index];
      if (callback) this.run(callback);
    }
    this.callbacks.length = 0;
  }

  private run(callback: DisposeCallback): void {
    try {
      const returned = callback() as unknown;
      if (isThenable(returned)) {
        void Promise.resolve(returned).catch((error: unknown) => this.report(error));
      }
    } catch (error) {
      this.report(error);
    }
  }

  private report(error: unknown): void {
    try {
      this.onError(error);
    } catch {
      // Error reporting is observational and must not break remaining cleanup.
    }
  }
}

/** First-terminal-wins settlement coupled to synchronous resource disposal. */
export class TerminalLatch<T> {
  private readonly disposables: DisposableStack;
  private readonly resolveCompletion: (value: T | PromiseLike<T>) => void;
  private isTerminal = false;
  private terminalValue: T | undefined;
  public readonly completion: Promise<T>;

  public constructor(onDisposalError: DisposalErrorHandler = ignoreDisposalError) {
    this.disposables = new DisposableStack(onDisposalError);
    let resolveCompletion: ((value: T | PromiseLike<T>) => void) | undefined;
    this.completion = new Promise<T>((resolve) => {
      resolveCompletion = resolve;
    });
    this.resolveCompletion = resolveCompletion as (value: T | PromiseLike<T>) => void;
  }

  public get terminal(): boolean {
    return this.isTerminal;
  }

  public get value(): T | undefined {
    return this.terminalValue;
  }

  public get signal(): AbortSignal {
    return this.disposables.signal;
  }

  public onDispose(callback: DisposeCallback): void {
    this.disposables.onDispose(callback);
  }

  public trySettle(value: T): boolean {
    if (this.isTerminal) return false;
    this.isTerminal = true;
    this.terminalValue = value;
    this.disposables.dispose();
    this.resolveCompletion(value);
    return true;
  }
}
