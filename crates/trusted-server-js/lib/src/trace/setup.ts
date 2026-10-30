import { validateTraceRequestContext } from './context';
import { changeTraceSession, type TraceSessionAction } from './lifecycle';
import type { CookieHealth } from './types';

const REPRODUCE =
  'Return to the affected page, reload once, reproduce the problem, then select View trace results.';
const RECOVERY =
  'Reopen the affected article on the exact same hostname and in this same tab, then reload once.';
const ON = 'Tracing is on — cookie observed by server';
const OFF = 'Tracing is off — no valid diagnostics session observed';

function healthLabel(health: CookieHealth): string {
  switch (health.state) {
    case 'absent':
      return 'Not present in this request';
    case 'present_valid':
      return 'Valid shape observed';
    case 'present_invalid':
      return 'Invalid shape observed';
    case 'duplicate':
      return 'Multiple values observed';
    case 'unavailable':
      return health.detail === 'runtime_header_ambiguous'
        ? 'Unavailable — runtime-visible cookies could not be reliably inspected'
        : 'Unavailable — runtime-visible cookie inspection failed';
  }
}

function fact(root: Document, list: Element, name: string, value: string): void {
  const term = root.createElement('dt');
  term.textContent = name;
  const description = root.createElement('dd');
  description.textContent = value;
  list.append(term, description);
}

function setupFacts(root: Document): void {
  const source = root.getElementById('trace-request-context');
  const network = root.getElementById('trace-network-facts');
  const cookies = root.getElementById('trace-cookie-facts');
  if (!source || !network || !cookies) return;
  let context: unknown;
  try {
    context = JSON.parse(source.textContent ?? '');
  } catch {
    context = undefined;
  }
  source.textContent = '';
  network.replaceChildren();
  cookies.replaceChildren();
  if (!validateTraceRequestContext(context)) {
    fact(root, network, 'Request facts', 'Unavailable');
    fact(root, cookies, 'Cookie health', 'Unavailable');
    return;
  }
  const facts = context.network;
  fact(root, network, 'Approximate network identifier', facts.masked_client_ip ?? 'Unavailable');
  fact(root, network, 'Country', facts.country ?? 'Unavailable');
  fact(root, network, 'Region', facts.region ?? 'Unavailable');
  fact(root, network, 'ASN', facts.asn === undefined ? 'Unavailable' : String(facts.asn));
  fact(root, network, 'HTTP version', facts.http_version ?? 'Unavailable');
  fact(root, network, 'TLS protocol', facts.tls_protocol ?? 'Unavailable');
  fact(root, network, 'TLS cipher', facts.tls_cipher ?? 'Unavailable');
  fact(root, network, 'Edge hostname', facts.edge_hostname ?? 'Unavailable');
  fact(root, network, 'Edge region', facts.edge_region ?? 'Unavailable');
  fact(root, network, 'Edge POP', facts.edge_pop ?? 'Unavailable');
  fact(root, cookies, 'Edge Cookie', healthLabel(context.cookies.ts_ec));
  fact(root, cookies, 'External IDs', healthLabel(context.cookies.ts_eids));
  fact(root, cookies, 'Tester', healthLabel(context.cookies.ts_tester));
  fact(root, cookies, 'Diagnostics session', healthLabel(context.cookies.diagnostics_session));
}

/** Mounts explicit setup controls without changing cookies on page load. */
export function mountTraceSetup(root: Document = document): () => void {
  setupFacts(root);
  const state = root.getElementById('trace-session-state');
  const status = root.getElementById('trace-status');
  const enable = root.getElementById('trace-enable');
  const end = root.getElementById('trace-end');
  const back = root.getElementById('trace-back');
  if (
    !state ||
    !status ||
    !(enable instanceof HTMLButtonElement) ||
    !(end instanceof HTMLButtonElement) ||
    !(back instanceof HTMLButtonElement)
  )
    return () => undefined;
  state.textContent =
    state.dataset.observedActive === 'true'
      ? ON
      : state.dataset.observedActive === 'false'
        ? OFF
        : 'Tracing state unconfirmed';
  let pending = false;
  let destroyed = false;
  const action = async (requested: TraceSessionAction): Promise<void> => {
    if (destroyed || pending) return;
    pending = true;
    enable.disabled = end.disabled = true;
    status.textContent =
      requested === 'enable' ? 'Verifying activation…' : 'Verifying deactivation…';
    try {
      const result = await changeTraceSession(requested);
      if (destroyed) return;
      state.textContent =
        result.observation === 'active'
          ? ON
          : result.observation === 'inactive'
            ? OFF
            : 'Tracing state unconfirmed';
      if (result.confirmed) {
        status.textContent = requested === 'enable' ? REPRODUCE : OFF;
        back.classList.toggle('primary', requested === 'enable');
      } else {
        status.textContent = `${requested === 'enable' ? 'Activation' : 'Deactivation'} unconfirmed. Try again.`;
      }
    } finally {
      if (!destroyed) enable.disabled = end.disabled = false;
      pending = false;
    }
  };
  const enableClick = (): void => {
    void action('enable');
  };
  const endClick = (): void => {
    void action('end');
  };
  const backClick = (): void => {
    if (destroyed) return;
    if (window.history.length > 1) window.history.back();
    else status.textContent = RECOVERY;
  };
  enable.addEventListener('click', enableClick);
  end.addEventListener('click', endClick);
  back.addEventListener('click', backClick);
  return () => {
    destroyed = true;
    enable.removeEventListener('click', enableClick);
    end.removeEventListener('click', endClick);
    back.removeEventListener('click', backClick);
  };
}
