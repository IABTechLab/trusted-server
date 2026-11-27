// Shared helpers for the identity smoke-test scripts: argument parsing and a
// minimal Chrome DevTools Protocol client over Node's built-in WebSocket (Node 22+).

/**
 * Parse `--key value` pairs (repeatable keys collect into arrays) and positionals.
 * @param {string[]} argv
 */
export function parseArgs(argv) {
  const options = {};
  const positionals = [];
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg.startsWith('--')) {
      const key = arg.slice(2);
      const value = argv[index + 1] && !argv[index + 1].startsWith('--') ? argv[(index += 1)] : 'true';
      options[key] = key in options ? [].concat(options[key], value) : value;
    } else {
      positionals.push(arg);
    }
  }
  return { options, positionals };
}

/** Return a list for an option that may be given once, many times or not at all. */
export function list(value) {
  return value === undefined ? [] : [].concat(value);
}

/**
 * Connect to the first page whose URL is on `host` (else the first page).
 * @param {{cdp: string, host?: string}} target
 */
export async function connect({ cdp, host }) {
  const response = await fetch(`${cdp}/json/list`);
  const targets = await response.json();
  const pages = targets.filter((entry) => entry.type === 'page');
  const page = (host && pages.find((entry) => entry.url.startsWith(`https://${host}`))) ?? pages[0];
  if (!page) throw new Error(`no page target at ${cdp}; is Chrome running with --remote-debugging-port?`);

  const socket = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    socket.onopen = resolve;
    socket.onerror = () => reject(new Error(`cannot open ${page.webSocketDebuggerUrl}`));
  });

  let nextId = 1;
  const pending = new Map();
  const listeners = [];
  socket.onmessage = (event) => {
    const message = JSON.parse(event.data);
    if (message.id && pending.has(message.id)) {
      pending.get(message.id)(message);
      pending.delete(message.id);
      return;
    }
    for (const listener of listeners) listener(message);
  };

  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = nextId;
      nextId += 1;
      const timer = setTimeout(() => reject(new Error(`${method} timed out`)), 60000);
      pending.set(id, (message) => {
        clearTimeout(timer);
        if (message.error) reject(new Error(`${method}: ${message.error.message}`));
        else resolve(message.result);
      });
      socket.send(JSON.stringify({ id, method, params }));
    });

  return { send, on: (listener) => listeners.push(listener), close: () => socket.close(), url: page.url };
}

/** Evaluate an expression in the page and return its JSON value (promises are awaited). */
export async function evaluate(session, expression) {
  const result = await session.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? 'evaluation failed');
  return result.result.value;
}

export const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
