/** Deliberate server action requested by the trace setup and cleanup controls. */
export type TraceSessionAction = 'enable' | 'end';

/** Separates requested mutation from what the follow-up request observed. */
export type TraceSessionChangeResult = Readonly<{
  mutation: 'requested' | 'failed';
  observation: 'active' | 'inactive' | 'failed' | 'not_attempted';
  confirmed: boolean;
}>;

/** Requests an explicit cookie change and verifies the resulting request state. */
export async function changeTraceSession(
  action: TraceSessionAction
): Promise<TraceSessionChangeResult> {
  return requestAndObserve(action, false);
}

/** Attempts end and a separate state observation even when the end request fails. */
export async function endTraceSessionAndObserve(): Promise<TraceSessionChangeResult> {
  return requestAndObserve('end', true);
}

async function requestAndObserve(
  action: TraceSessionAction,
  observeFailedMutation: boolean
): Promise<TraceSessionChangeResult> {
  const failed: TraceSessionChangeResult = {
    mutation: 'failed',
    observation: 'not_attempted',
    confirmed: false,
  };
  if (action !== 'enable' && action !== 'end') return failed;

  let mutation: TraceSessionChangeResult['mutation'] = 'failed';
  try {
    const response = await fetch(`/_ts/trace/${action}`, {
      method: 'POST',
      credentials: 'same-origin',
      cache: 'no-store',
      headers: { 'X-TS-Trace-Action': action },
    });
    if (response.ok) mutation = 'requested';
  } catch {
    /* The cleanup caller still attempts its independent state observation. */
  }
  if (mutation === 'failed' && !observeFailedMutation) return failed;

  const unconfirmed: TraceSessionChangeResult = {
    mutation,
    observation: 'failed',
    confirmed: false,
  };
  try {
    const response = await fetch('/_ts/trace/state', {
      method: 'GET',
      credentials: 'same-origin',
      cache: 'no-store',
    });
    if (!response.ok) return unconfirmed;
    const state: unknown = await response.json();
    if (state === null || typeof state !== 'object' || Array.isArray(state)) {
      return unconfirmed;
    }
    const keys = Object.keys(state);
    if (keys.length !== 1 || keys[0] !== 'observed_active') return unconfirmed;
    const observed = Object.getOwnPropertyDescriptor(state, 'observed_active');
    if (!observed || typeof observed.value !== 'boolean') return unconfirmed;
    const active: boolean = observed.value;
    return {
      mutation,
      observation: active ? 'active' : 'inactive',
      confirmed: mutation === 'requested' && active === (action === 'enable'),
    };
  } catch {
    return unconfirmed;
  }
}
