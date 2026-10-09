import type { CookieHealth } from './types';

/** Shared DOM and label primitives for the trace viewer; all report text uses textContent. */
export function label(value: string): string {
  const labels: Record<string, string> = {
    no_bid: 'No bid returned',
    no_candidate: 'No candidate',
    selected: 'Candidate selected',
    selected_unrenderable: 'Selected candidate could not be rendered',
    trusted_server_direct: 'Trusted Server request path observed',
    prebid_refresh: 'Browser refresh observed; winner not determined',
    publisher_refresh: 'Browser refresh observed; winner not determined',
    competing: 'Multiple or unknown delivery paths',
    unattributed: 'Multiple or unknown delivery paths',
    trusted_server_response_sent: 'Trusted Server creative response sent',
    trusted_server_selected: 'Trusted Server candidate selected; render unconfirmed',
    candidate_unconfirmed: 'Candidate unconfirmed',
    not_observed: 'Not observed',
    unknown: 'Unknown',
    unavailable: 'Unavailable',
  };
  return (
    labels[value] ??
    value
      .replace(/([a-z])([A-Z])/g, '$1 $2')
      .replace(/_/g, ' ')
      .replace(/^./, (first) => first.toUpperCase())
  );
}
export function valueText(value: unknown): string {
  if (value === undefined) return 'Unavailable';
  if (typeof value === 'boolean') return value ? 'Yes' : 'No';
  if (typeof value === 'number') return String(value);
  if (typeof value === 'string') return value;
  return 'Unavailable';
}
export function healthText(health: CookieHealth): string {
  if (health.state === 'absent') return 'Not present in this request';
  if (health.state === 'present_valid') return 'Valid shape observed';
  if (health.state === 'duplicate') return 'Multiple values observed';
  if (health.state === 'present_invalid')
    return health.detail === 'oversized'
      ? 'Invalid shape — too long'
      : health.detail === 'unsupported_value'
        ? 'Invalid shape — unsupported value'
        : 'Invalid shape observed';
  if (health.detail === 'runtime_header_ambiguous')
    return 'Unavailable — runtime-visible cookies could not be reliably inspected';
  return health.detail === 'header_too_large'
    ? 'Unavailable — the visible cookie header was too large'
    : 'Unavailable — the visible cookie header was not valid text';
}
export function element<K extends keyof HTMLElementTagNameMap>(
  root: Document,
  tag: K,
  text?: string
): HTMLElementTagNameMap[K] {
  const node = root.createElement(tag);
  if (text !== undefined) node.textContent = text;
  return node;
}
export function section(root: Document, parent: Element, title: string, id?: string): HTMLElement {
  const node = element(root, 'section');
  if (id) node.id = id;
  node.append(element(root, 'h2', title));
  parent.append(node);
  return node;
}
export function facts(
  root: Document,
  parent: Element,
  rows: readonly (readonly [string, unknown])[]
): void {
  const list = element(root, 'dl');
  const unavailable = element(root, 'dl');
  let unavailableCount = 0;
  for (const [name, value] of rows) {
    const text = valueText(value);
    const target = text === 'Unavailable' ? unavailable : list;
    if (target === unavailable) unavailableCount += 1;
    target.append(element(root, 'dt', name), element(root, 'dd', text));
  }
  if (list.childElementCount) parent.append(list);
  if (unavailableCount) {
    // Keep sparse fields reachable without making every slot scroll through them.
    const more = element(root, 'details');
    more.className = 'trace-unavailable';
    more.append(
      element(
        root,
        'summary',
        unavailableCount === 1 ? '1 field unavailable' : `${unavailableCount} fields unavailable`
      ),
      unavailable
    );
    parent.append(more);
  }
}
export function sizes(value?: readonly (readonly [number, number])[]): string {
  return value === undefined
    ? 'Unavailable'
    : value.length === 0
      ? 'Not observed'
      : value.map(([width, height]) => `${width} × ${height}`).join(', ');
}
export function millis(value?: number): string {
  // Browser clocks carry sub-millisecond float noise; one decimal stays readable.
  return value === undefined ? 'Unavailable' : `${Number(value.toFixed(1))} ms`;
}

/** A collapsed, titled detail section; titles stay visible to Find and screen readers. */
export function collapsible(
  root: Document,
  parent: Element,
  title: string,
  id?: string
): HTMLElement {
  const node = element(root, 'details');
  node.className = 'trace-section';
  if (id) node.id = id;
  const summary = element(root, 'summary');
  summary.append(element(root, 'h2', title));
  node.append(summary);
  parent.append(node);
  return node;
}

/** Formats an ISO capture instant as a readable UTC date and time; other text passes through. */
export function capturedLabel(value: string): string {
  const match = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2}:\d{2})/.exec(value);
  return match ? `${match[1]} ${match[2]} UTC` : value;
}
